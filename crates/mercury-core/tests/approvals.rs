//! Sign-off through the API: the pending list, approving and rejecting, nobody approving their own
//! question nor reading its draft, someone who doesn't approve the project, a flag on a role that
//! can't approve, expiry, the asker's stream going on after the decision, and asking again after a
//! rejection.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use chrono::TimeDelta;
use http_body_util::BodyExt;
use mercury_core::approvals::{self, Held};
use mercury_core::db::{self, Db};
use mercury_core::questions::{Asked, Emitter, FOLLOWS_REFUSED, Hub, Questions, Runner};
use mercury_core::{admin, app, audit};
use mercury_schema::api::{Answer, Flag, Refusal, RefusalCode, SignoffReason, Usd};
use mercury_schema::events::{Classified, ComposerMode, Event, ProjectPicked, ProjectSource, Refused};
use mercury_schema::facts::Ratio;
use mercury_schema::{CheckId, ProjectId, QuestionClass, Role};
use serde_json::{Value, json};
use tower::ServiceExt;

const DRAFT: &str = "Export invoices to CSV is done. The export part is tested too.";
const NOTE: &str = "Ask the tech lead about the export first.";
/// A question the runner refuses at once, for another reason than an approver's.
const ASKS_FOR_CODE: &str = "Show me the export code.";

/// Answers every question with the same held draft, after its project and class; refuses
/// [`ASKS_FOR_CODE`] before anything is held.
struct Holds {
    db: Db,
}

#[async_trait]
impl Runner for Holds {
    async fn run(&self, question: Asked, mut events: Emitter) {
        if question.text == ASKS_FOR_CODE {
            let refusal = Refusal {
                code: RefusalCode::AsksForCode,
                message: "Mercury doesn't show code.".into(),
                hint: "Ask how far along the export is.".into(),
            };
            let refused = Refused::new(refusal, Usd::default(), None);
            let _sent = events.emit(Event::Refused(refused)).await.unwrap();
            return;
        }
        let first = [
            Event::Project(ProjectPicked {
                project: ProjectId::parse("delta").unwrap(),
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
        let held = Held {
            reason: SignoffReason::ReviewBand,
            draft: draft(),
            flags: vec![Flag {
                sentence: "The export part is tested too.".into(),
                check: CheckId::parse("sentence_contains_code_or_identifier").unwrap(),
                p: Ratio::new(0.6).unwrap(),
            }],
            composer: ComposerMode::Model,
        };
        let _held = approvals::hold(&self.db, &mut events, held, db::now())
            .await
            .unwrap();
        // what the orchestrator spent until the hold, as its row holds it
        sqlx::query("UPDATE questions SET cost_usd = 0.004, judge_mode = 'jev' WHERE id = ?")
            .bind(events.question().as_str())
            .execute(self.db.pool())
            .await
            .unwrap();
    }
}

fn draft() -> Answer {
    Answer {
        text: DRAFT.into(),
        facts: Vec::new(),
        confidence: Ratio::new(0.81).unwrap(),
    }
}

struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    hub: Hub,
    app: Router,
    /// A project manager who also approves delta: she asks.
    anna: String,
    /// A tech lead who approves delta.
    tara: String,
    /// A project manager who approves nothing.
    ben: String,
}

async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let known = admin::KnownProjects::new(["delta"]);
    let mut tokens = Vec::new();
    for (email, role, approves) in [
        ("anna@example.com", Role::Pm, true),
        ("tara@example.com", Role::TechLead, true),
        ("ben@example.com", Role::Pm, false),
    ] {
        admin::add_user(&db, email, "Dana Reyes", role, now)
            .await
            .unwrap();
        admin::grant(&db, email, &["delta".to_owned()], approves, &known, now)
            .await
            .unwrap();
        let token = admin::create_token(&db, email, "laptop", TimeDelta::days(30), now)
            .await
            .unwrap();
        tokens.push(token.token.reveal());
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let questions = Questions::new(db.clone(), Arc::new(config), Arc::new(Holds { db: db.clone() }));
    let hub = questions.hub.clone();
    let ben = tokens.pop().unwrap();
    let tara = tokens.pop().unwrap();
    let anna = tokens.pop().unwrap();
    Core {
        _dir: dir,
        db,
        hub,
        app: app(questions),
        anna,
        tara,
        ben,
    }
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

async fn get(core: &Core, uri: &str, token: &str) -> (StatusCode, Value) {
    send(core, request("GET", uri, token).body(Body::empty()).unwrap()).await
}

async fn decide(core: &Core, approval: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let request = request("POST", &format!("/v1/approvals/{approval}"), token)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(core, request).await
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

/// The asker's stream after `last`, read to its end.
async fn follow(app: Router, token: String, id: String, last: Option<u64>) -> Vec<Sent> {
    let mut builder = request("GET", &format!("/v1/questions/{id}/events"), &token);
    if let Some(last) = last {
        builder = builder.header("last-event-id", last.to_string());
    }
    let response = app.oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(Duration::from_secs(10), response.into_body().collect())
        .await
        .expect("the stream ends at its terminal event")
        .unwrap()
        .to_bytes();
    parse(std::str::from_utf8(&bytes).unwrap())
}

fn names(sent: &[Sent]) -> Vec<&str> {
    sent.iter().map(|(_, name, _)| name.as_str()).collect()
}

/// `POST /v1/questions` with `body`, by the holder of `token`, with a fresh key.
async fn ask(core: &Core, token: &str, body: Value) -> (StatusCode, Value) {
    static KEYS: AtomicU64 = AtomicU64::new(0);
    let key = KEYS.fetch_add(1, Ordering::Relaxed);
    let request = request("POST", "/v1/questions", token)
        .header(CONTENT_TYPE, "application/json")
        .header(
            "idempotency-key",
            format!("k-{}-{key}", db::now().timestamp_nanos_opt().unwrap()),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    send(core, request).await
}

/// Anna asks; the question waits for sign-off. Its id, its approval's id and the `seq` of its
/// `pending_signoff`.
async fn held(core: &Core) -> (String, String, u64) {
    held_for(core, &core.anna).await
}

/// The holder of `token` asks; the question waits for sign-off, as [`held`] says.
async fn held_for(core: &Core, token: &str) -> (String, String, u64) {
    let (status, body) = ask(
        core,
        token,
        json!({ "text": "What is done?", "project": "delta" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["id"].as_str().unwrap().to_owned();
    let sent = follow(core.app.clone(), token.to_owned(), id.clone(), None).await;
    assert_eq!(
        names(&sent),
        ["accepted", "project", "classified", "pending_signoff"],
        "the stream ends at the hold"
    );
    let (seq, _, pending) = sent.last().unwrap();
    assert_eq!(pending["reason"], "review_band");
    let approval = pending["approval_id"].as_str().unwrap().to_owned();
    // the runner's last write lands after the hold: wait for it, as a client would read the state
    for _ in 0..50 {
        let cost: f64 = sqlx::query_scalar("SELECT cost_usd FROM questions WHERE id = ?")
            .bind(&id)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
        if cost > 0.0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (id, approval, *seq)
}

/// The audit rows of this approval's decision, and of the question's events after the hold.
async fn rows_after(db: &Db, seq: u64) -> Vec<String> {
    let after = i64::try_from(seq).unwrap();
    sqlx::query_scalar("SELECT type FROM audit_events WHERE seq > ? ORDER BY seq")
        .bind(after)
        .fetch_all(db.pool())
        .await
        .unwrap()
}

/// An approver sees the draft and its flags; approving delivers the draft byte for byte, to a
/// stream that waited after the hold and to the question's state, after the decision's own row.
#[tokio::test]
async fn approving_delivers_the_draft_unchanged_to_the_waiting_stream() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;

    let (status, state) = get(&core, &format!("/v1/questions/{id}"), &core.anna).await;
    assert_eq!(
        (status, state["status"].as_str()),
        (StatusCode::OK, Some("pending_signoff"))
    );
    assert_eq!(state["answer"], Value::Null, "nothing delivered while it waits");

    let (status, list) = get(&core, "/v1/approvals?status=pending", &core.tara).await;
    assert_eq!(status, StatusCode::OK);
    let item = &list["approvals"][0];
    assert_eq!(list["approvals"].as_array().unwrap().len(), 1);
    assert_eq!(item["id"], approval);
    assert_eq!(item["question_id"], id);
    assert_eq!(item["project"], "delta");
    assert_eq!(item["class"], "progress");
    assert_eq!(item["asker_role"], "pm");
    assert_eq!(item["reason"], "review_band");
    assert_eq!(item["draft"], serde_json::to_value(draft()).unwrap());
    assert_eq!(
        item["flags"],
        json!([{ "sentence": "The export part is tested too.", "check": "sentence_contains_code_or_identifier", "p": 0.6 }])
    );
    assert_eq!(
        (item["asked_by_me"].as_bool(), item["status"].as_str()),
        (Some(false), Some("pending"))
    );

    // the asker's stream comes back after the hold and waits for the decision
    let waiting = tokio::spawn(follow(
        core.app.clone(),
        core.anna.clone(),
        id.clone(),
        Some(pending),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, _) = decide(&core, &approval, &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let rest = waiting.await.unwrap();
    assert_eq!(names(&rest), ["answer", "done"]);
    assert_eq!(
        rest[0].2,
        serde_json::to_value(draft()).unwrap(),
        "byte for byte the draft"
    );
    let done = &rest[1].2;
    assert_eq!(
        done["audit_id"].as_u64(),
        Some(rest[0].0),
        "done names the answer's row"
    );
    assert_eq!(
        (done["cost_usd"].as_f64(), done["judge_mode"].as_str()),
        (Some(0.004), Some("jev"))
    );
    assert_eq!(done["composer"], "model");

    // the decision's own row comes first, without a question of its own
    assert_eq!(
        rows_after(&core.db, pending).await,
        ["approval_decided", "answer", "done"]
    );
    let (decided, question): (String, Option<String>) =
        sqlx::query_as("SELECT payload_json, question_id FROM audit_events WHERE type = 'approval_decided'")
            .fetch_one(core.db.pool())
            .await
            .unwrap();
    let decided: Value = serde_json::from_str(&decided).unwrap();
    assert_eq!(question, None);
    assert_eq!(
        (decided["decision"].as_str(), decided["question_id"].as_str()),
        (Some("approved"), Some(id.as_str()))
    );

    // a replay from the start goes past the hold to the answer
    let replay = follow(core.app.clone(), core.anna.clone(), id.clone(), None).await;
    assert_eq!(
        names(&replay),
        [
            "accepted",
            "project",
            "classified",
            "pending_signoff",
            "answer",
            "done"
        ]
    );
    let (_, state) = get(&core, &format!("/v1/questions/{id}"), &core.anna).await;
    assert_eq!(state["status"], "delivered");
    assert_eq!(state["answer"]["text"], DRAFT);
    assert_eq!(state["audit_id"].as_u64(), Some(rest[0].0));
    let (answer, finished): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT answer_redacted, finished_at FROM questions WHERE id = ?")
            .bind(&id)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
    assert_eq!(answer.as_deref(), Some(DRAFT));
    assert!(finished.is_some());

    // the first decision wins; the item is gone from the list
    let (status, body) = decide(
        &core,
        &approval,
        &core.tara,
        json!({ "decision": "reject", "note": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, list) = get(&core, "/v1/approvals", &core.tara).await;
    assert_eq!(list, json!({ "approvals": [] }));
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

/// A rejection needs a note, and the asker gets it as the refusal's hint.
#[tokio::test]
async fn a_rejection_needs_a_note_and_the_asker_gets_it_as_the_hint() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;

    for body in [
        json!({ "decision": "reject" }),
        json!({ "decision": "reject", "note": "   " }),
        json!({ "decision": "reject", "note": "x".repeat(501) }),
        json!({ "decision": "reject", "note": "two\nlines" }),
        json!({ "decision": "maybe" }),
        json!({ "decision": "approve", "edit": "a better draft" }),
    ] {
        let (status, error) = decide(&core, &approval, &core.tara, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(error["error"]["code"], "invalid_request");
    }
    let (status, _) = decide(
        &core,
        &approval,
        &core.tara,
        json!({ "decision": "reject", "note": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let rest = follow(core.app.clone(), core.anna.clone(), id.clone(), Some(pending)).await;
    assert_eq!(names(&rest), ["refused"]);
    let refused = &rest[0].2;
    assert_eq!(refused["code"], "rejected_by_approver");
    assert_eq!(refused["hint"], NOTE);
    assert_eq!(
        (refused["cost_usd"].as_f64(), refused["judge_mode"].as_str()),
        (Some(0.004), Some("jev"))
    );
    assert_eq!(
        rows_after(&core.db, pending).await,
        ["approval_decided", "refused"]
    );
    let (_, state) = get(&core, &format!("/v1/questions/{id}"), &core.anna).await;
    assert_eq!(state["status"], "refused");
    assert_eq!(state["refusal"]["hint"], NOTE);
    assert_eq!(state["answer"], Value::Null);
    let (status, note): (String, Option<String>) =
        sqlx::query_as("SELECT status, note FROM approvals WHERE id = ?")
            .bind(&approval)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
    assert_eq!((status.as_str(), note.as_deref()), ("rejected", Some(NOTE)));
}

/// Nobody approves their own question, whatever their flags: the list says it's theirs, and a
/// decision on it is refused without changing anything.
#[tokio::test]
async fn nobody_approves_their_own_question() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;

    let (_, list) = get(&core, "/v1/approvals?status=pending", &core.anna).await;
    assert_eq!(list["approvals"][0]["asked_by_me"], true);
    for body in [
        json!({ "decision": "approve" }),
        json!({ "decision": "reject", "note": NOTE }),
    ] {
        let (status, error) = decide(&core, &approval, &core.anna, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["error"]["code"], "invalid_request");
    }
    assert_eq!(
        rows_after(&core.db, pending).await,
        Vec::<String>::new(),
        "nothing written"
    );
    let (_, state) = get(&core, &format!("/v1/questions/{id}"), &core.anna).await;
    assert_eq!(state["status"], "pending_signoff");
}

/// The asker's own item comes without its draft and its flags, whatever the client: both fields
/// are left out of the body, not emptied. Another approver's item of the same question has both.
#[tokio::test]
async fn the_askers_own_item_comes_without_its_draft_and_flags() {
    let core = core().await;
    let (id, approval, _) = held(&core).await;

    let (status, mine) = get(&core, "/v1/approvals?status=pending", &core.anna).await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(mine["approvals"].as_array().map(Vec::len), Some(1));
    let own = mine["approvals"][0].as_object().unwrap();
    assert_eq!(own["id"], approval.as_str());
    assert_eq!(own["question_id"], id.as_str());
    assert_eq!(own["asked_by_me"], true);
    assert_eq!(
        (own["reason"].as_str(), own["status"].as_str()),
        (Some("review_band"), Some("pending"))
    );
    assert!(
        !own.contains_key("draft") && !own.contains_key("flags"),
        "left out: {own:?}"
    );
    let body = mine.to_string();
    assert!(
        !body.contains(DRAFT) && !body.contains("tested too"),
        "nothing of the draft: {body}"
    );

    let (status, theirs) = get(&core, "/v1/approvals?status=pending", &core.tara).await;
    assert_eq!(status, StatusCode::OK, "{theirs}");
    let item = &theirs["approvals"][0];
    assert_eq!(item["question_id"], id.as_str());
    assert_eq!(item["asked_by_me"], false);
    assert_eq!(item["draft"], serde_json::to_value(draft()).unwrap());
    assert_eq!(item["flags"].as_array().map(Vec::len), Some(1));
}

/// A question an approver rejected may be asked again naming it: the new question runs as any
/// other, and its `accepted` row, its first event, carries `follows`. One nobody decided on within
/// the day ended the same way, and may be asked again too.
#[tokio::test]
async fn a_rejected_question_asked_again_names_it_in_its_accepted_row() {
    let core = core().await;
    let (rejected, approval, _) = held(&core).await;
    let (status, _) = decide(
        &core,
        &approval,
        &core.tara,
        json!({ "decision": "reject", "note": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let again = json!({ "text": "What is done in billing?", "project": "delta", "follows": rejected });
    let (status, body) = ask(&core, &core.anna, again).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["id"].as_str().unwrap().to_owned();
    assert_ne!(id, rejected);
    assert_eq!(body, json!({ "id": id }), "the 202 is the id alone");

    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM audit_events WHERE question_id = ? AND type = 'accepted'",
    )
    .bind(&id)
    .fetch_one(core.db.pool())
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload, json!({ "id": id, "follows": rejected }));
    // it runs as any other question, every check included: this one is held again
    let sent = follow(core.app.clone(), core.anna.clone(), id.clone(), None).await;
    assert_eq!(
        names(&sent),
        ["accepted", "project", "classified", "pending_signoff"]
    );
    assert_eq!(sent[0].2, payload, "its first event is that row");

    let (expired, _, _) = held(&core).await;
    let later = db::now() + TimeDelta::hours(24) + TimeDelta::seconds(1);
    assert!(approvals::expire_due(&core.db, &core.hub, later).await.unwrap() >= 1);
    let after_expiry = json!({ "text": "What is done?", "project": "delta", "follows": expired });
    let (status, body) = ask(&core, &core.anna, after_expiry).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

/// A `follows` naming anything but the caller's own question that an approver rejected is
/// refused before a question starts, with one and the same answer whatever the reason: an
/// unknown id, someone else's rejected question, one of the caller's still waiting, one
/// delivered, one refused for another reason.
#[tokio::test]
async fn a_follows_that_isnt_the_callers_rejected_question_is_refused_the_same_way() {
    let core = core().await;
    let (someone_elses, approval, _) = held_for(&core, &core.ben).await;
    let (status, _) = decide(
        &core,
        &approval,
        &core.tara,
        json!({ "decision": "reject", "note": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (waiting, _, _) = held(&core).await;
    let (delivered, approval, _) = held(&core).await;
    let (status, _) = decide(&core, &approval, &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = ask(
        &core,
        &core.anna,
        json!({ "text": ASKS_FOR_CODE, "project": "delta" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let refused = body["id"].as_str().unwrap().to_owned();
    let sent = follow(core.app.clone(), core.anna.clone(), refused.clone(), None).await;
    assert_eq!(names(&sent), ["accepted", "refused"]);
    assert_eq!(sent[1].2["code"], "asks_for_code");

    // a second token of the asker's: each token has its own rate limit, and she has asked often
    let phone = admin::create_token(
        &core.db,
        "anna@example.com",
        "phone",
        TimeDelta::days(30),
        db::now(),
    )
    .await
    .unwrap()
    .token
    .reveal();
    let before = questions_asked(&core.db).await;
    let mut answers = Vec::new();
    for follows in [
        "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
        someone_elses.as_str(),
        waiting.as_str(),
        delivered.as_str(),
        refused.as_str(),
    ] {
        let body = json!({ "text": "What is done?", "project": "delta", "follows": follows });
        let (status, error) = ask(&core, &phone, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{follows}: {error}");
        assert_eq!(error["error"]["code"], "invalid_request", "{follows}");
        answers.push(error);
    }
    assert!(
        answers.windows(2).all(|pair| pair[0] == pair[1]),
        "one answer whatever the reason: {answers:#?}"
    );
    assert_eq!(answers[0]["error"]["message"], FOLLOWS_REFUSED.0);
    assert_eq!(answers[0]["error"]["hint"], FOLLOWS_REFUSED.1);
    assert_eq!(questions_asked(&core.db).await, before, "no question started");
}

async fn questions_asked(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM questions")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Someone who doesn't approve the project sees nothing to approve, and can't tell an approval
/// of it from one that doesn't exist.
#[tokio::test]
async fn someone_who_doesnt_approve_the_project_sees_and_decides_nothing() {
    let core = core().await;
    let (_, approval, pending) = held(&core).await;

    let (status, list) = get(&core, "/v1/approvals?status=pending", &core.ben).await;
    assert_eq!((status, list), (StatusCode::OK, json!({ "approvals": [] })));
    let (status, theirs) = decide(&core, &approval, &core.ben, json!({ "decision": "approve" })).await;
    let (_, unknown) = decide(
        &core,
        "ap_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
        &core.ben,
        json!({ "decision": "approve" }),
    )
    .await;
    let (_, malformed) = decide(&core, "nothing", &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(theirs, unknown, "the same answer either way");
    assert_eq!(theirs, malformed);
    assert_eq!(rows_after(&core.db, pending).await, Vec::<String>::new());

    let (status, _) = get(&core, "/v1/approvals?status=approved", &core.tara).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get(&core, "/v1/approvals", "mcy_nothing").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An approver flag counts only for a project manager or a tech lead. One left on a board or an
/// admin person, written straight into the database as a grant could write it before that rule,
/// grants nothing: their pending list is empty, a decision is refused as for a project they don't
/// approve, the project counts as having no approver for a review band, and `/v1/me` says
/// `approver: false`. `access list` still shows the stored flag, so it can be taken back.
#[tokio::test]
async fn a_flag_left_on_a_board_or_admin_person_grants_nothing() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;
    let now = db::now();
    let known = admin::KnownProjects::new(["delta", "gamma"]);
    let both = ["delta".to_owned(), "gamma".to_owned()];
    for (email, role) in [("bo@example.com", Role::Board), ("ada@example.com", Role::Admin)] {
        let user = admin::add_user(&core.db, email, "Dana Reyes", role, now)
            .await
            .unwrap();
        admin::grant(&core.db, email, &both, false, &known, now)
            .await
            .unwrap();
        sqlx::query("UPDATE user_projects SET approver = 1 WHERE user_id = ?")
            .bind(&user)
            .execute(core.db.pool())
            .await
            .unwrap();
        let token = admin::create_token(&core.db, email, "laptop", TimeDelta::days(30), now)
            .await
            .unwrap()
            .token
            .reveal();

        let (status, list) = get(&core, "/v1/approvals?status=pending", &token).await;
        assert_eq!(
            (status, list),
            (StatusCode::OK, json!({ "approvals": [] })),
            "{email}"
        );
        let (status, theirs) = decide(&core, &approval, &token, json!({ "decision": "approve" })).await;
        let (_, unknown) = decide(
            &core,
            "ap_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
            &token,
            json!({ "decision": "approve" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{email}");
        assert_eq!(theirs, unknown, "{email}: the same answer either way");

        let (status, me) = get(&core, "/v1/me", &token).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!(
            me["projects"],
            json!([{ "project": "delta", "approver": false }, { "project": "gamma", "approver": false }]),
            "{email}"
        );

        // the admin commands show the flag as stored
        let access = admin::access(&core.db, None, Some(email)).await.unwrap();
        assert!(
            access.people[0].grants.iter().all(|grant| grant.approver),
            "{email}: {access:?}"
        );
    }

    // nothing was decided: the item still waits, and its approver still has it
    assert!(
        !rows_after(&core.db, pending)
            .await
            .contains(&"approval_decided".to_owned())
    );
    let (_, state) = get(&core, &format!("/v1/questions/{id}"), &core.anna).await;
    assert_eq!(state["status"], "pending_signoff");
    let (_, list) = get(&core, "/v1/approvals", &core.tara).await;
    assert_eq!(list["approvals"][0]["id"], approval.as_str());

    // gamma's only flags are theirs, so a review band there has nobody to be held for; a tech
    // lead's flag on it is one
    let ben: String = sqlx::query_scalar("SELECT id FROM users WHERE email = 'ben@example.com'")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    let gamma = ProjectId::parse("gamma").unwrap();
    assert!(!approvals::has_approver(&core.db, &gamma, &ben).await.unwrap());
    admin::grant(
        &core.db,
        "tara@example.com",
        &["gamma".to_owned()],
        true,
        &known,
        now,
    )
    .await
    .unwrap();
    assert!(approvals::has_approver(&core.db, &gamma, &ben).await.unwrap());
}

/// An answer nobody decides on within a day is refused, and the asker is told to ask again; the
/// sweep and a late decision both end it, once.
#[tokio::test]
async fn an_answer_nobody_decides_on_is_refused_after_a_day() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;

    let now = db::now();
    assert_eq!(
        approvals::expire_due(&core.db, &core.hub, now).await.unwrap(),
        0,
        "not yet"
    );
    let (_, list) = get(&core, "/v1/approvals", &core.tara).await;
    assert_eq!(list["approvals"].as_array().unwrap().len(), 1);

    let later = now + TimeDelta::hours(24) + TimeDelta::seconds(1);
    assert_eq!(
        approvals::expire_due(&core.db, &core.hub, later).await.unwrap(),
        1
    );
    assert_eq!(
        approvals::expire_due(&core.db, &core.hub, later).await.unwrap(),
        0,
        "once"
    );

    let rest = follow(core.app.clone(), core.anna.clone(), id.clone(), Some(pending)).await;
    assert_eq!(names(&rest), ["refused"]);
    assert_eq!(rest[0].2["code"], "rejected_by_approver");
    assert_eq!(rest[0].2["message"], approvals::EXPIRED);
    assert_eq!(rest[0].2["hint"], approvals::EXPIRED_HINT);
    assert_eq!(
        rows_after(&core.db, pending).await,
        ["approval_expired", "refused"]
    );
    let status: String = sqlx::query_scalar("SELECT status FROM approvals WHERE id = ?")
        .bind(&approval)
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(status, "expired");
    let (status, _) = decide(&core, &approval, &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// A decision that comes after the day, before the sweep, finds the item expired: it is refused
/// as the sweep would have, and the decision gets `409`.
#[tokio::test]
async fn a_late_decision_finds_the_answer_expired() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;
    sqlx::query("UPDATE approvals SET expires_at = ? WHERE id = ?")
        .bind(db::time(db::now() - TimeDelta::seconds(1)))
        .bind(&approval)
        .execute(core.db.pool())
        .await
        .unwrap();

    let (_, list) = get(&core, "/v1/approvals", &core.tara).await;
    assert_eq!(list, json!({ "approvals": [] }), "an expired item isn't pending");
    let (status, error) = decide(&core, &approval, &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let rest = follow(core.app.clone(), core.anna.clone(), id, Some(pending)).await;
    assert_eq!(names(&rest), ["refused"]);
    assert_eq!(rest[0].2["message"], approvals::EXPIRED);
}

/// An approval whose stored reason and draft don't parse spoils nothing else: the list leaves it out
/// and shows the others, and the minute's sweep still refuses every item whose day is over, the
/// broken one first among them.
#[tokio::test]
async fn an_unreadable_approval_stops_neither_the_list_nor_the_sweep() {
    let core = core().await;
    let (first, broken, first_pending) = held(&core).await;
    let (second, readable, second_pending) = held(&core).await;
    sqlx::query("UPDATE approvals SET reason_json = 'not json', draft_answer = '{' WHERE id = ?")
        .bind(&broken)
        .execute(core.db.pool())
        .await
        .unwrap();

    let (status, list) = get(&core, "/v1/approvals?status=pending", &core.tara).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let ids: Vec<&str> = list["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [readable.as_str()],
        "the broken row is left out, the others listed"
    );

    // both due; the broken one comes first, and doesn't keep the other waiting
    sqlx::query("UPDATE approvals SET expires_at = ? WHERE id = ?")
        .bind(db::time(db::now() - TimeDelta::minutes(2)))
        .bind(&broken)
        .execute(core.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE approvals SET expires_at = ? WHERE id = ?")
        .bind(db::time(db::now() - TimeDelta::minutes(1)))
        .bind(&readable)
        .execute(core.db.pool())
        .await
        .unwrap();
    assert_eq!(
        approvals::expire_due(&core.db, &core.hub, db::now())
            .await
            .unwrap(),
        2
    );
    for (id, pending) in [(&first, first_pending), (&second, second_pending)] {
        let rest = follow(core.app.clone(), core.anna.clone(), id.clone(), Some(pending)).await;
        assert_eq!(names(&rest), ["refused"], "{id}");
        assert_eq!(rest[0].2["message"], approvals::EXPIRED);
    }
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

/// A rejection reads neither the draft nor the reason, so an item whose stored draft is broken
/// can still be refused by its approver.
#[tokio::test]
async fn an_item_whose_draft_cant_be_read_can_still_be_rejected() {
    let core = core().await;
    let (id, approval, pending) = held(&core).await;
    sqlx::query("UPDATE approvals SET reason_json = 'not json', draft_answer = '{' WHERE id = ?")
        .bind(&approval)
        .execute(core.db.pool())
        .await
        .unwrap();
    let (status, _) = decide(
        &core,
        &approval,
        &core.tara,
        json!({ "decision": "reject", "note": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rest = follow(core.app.clone(), core.anna.clone(), id, Some(pending)).await;
    assert_eq!(names(&rest), ["refused"]);
    assert_eq!(rest[0].2["hint"], NOTE);
}

/// A decision's row names who decided and with which token, so the audit's `user` and `token`
/// filters find a sign-off as they find a sign-in.
#[tokio::test]
async fn a_decision_names_who_decided_so_the_audit_filters_find_it() {
    let core = core().await;
    let (_, approval, _) = held(&core).await;
    let (status, _) = decide(&core, &approval, &core.tara, json!({ "decision": "approve" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // the audit is an admin's
    let now = db::now();
    admin::add_user(&core.db, "admin@example.com", "Dana Reyes", Role::Admin, now)
        .await
        .unwrap();
    let admin_token = admin::create_token(&core.db, "admin@example.com", "laptop", TimeDelta::days(30), now)
        .await
        .unwrap()
        .token
        .reveal();
    let public_id = core.tara.split('_').nth(1).unwrap().to_owned();
    for filter in [format!("token={public_id}"), "user=TARA@example.com".to_owned()] {
        let (status, page) = get(&core, &format!("/v1/audit?{filter}"), &admin_token).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let decided: Vec<&Value> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["type"] == "approval_decided")
            .collect();
        assert_eq!(decided.len(), 1, "{filter}: {page}");
        assert_eq!(decided[0]["user"], "tara@example.com", "{filter}");
        assert_eq!(decided[0]["token"], public_id.as_str(), "{filter}");
        assert_eq!(decided[0]["payload"]["approval_id"], approval.as_str());
    }
}
