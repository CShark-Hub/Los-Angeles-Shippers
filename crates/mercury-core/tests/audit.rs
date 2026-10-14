//! The audit API: who sees which rows, the filters, the cursor, the export as an admin's audited
//! action whose file verifies without the database, and retention keeping the chain.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use clap::Parser as _;
use http_body_util::BodyExt;
use mercury_core::audit::{self, Retained};
use mercury_core::cli::{Cli, Command};
use mercury_core::db::{self, Db};
use mercury_core::questions::{NotBuilt, Questions};
use mercury_core::{admin, app};
use mercury_schema::api::{Accepted, AuditPage, ErrorCode};
use mercury_schema::events::Event;
use mercury_schema::{ProjectId, QuestionId, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

// ------------------------------------------------------------------------------------ the setup

/// Two people who ask (`anna` on delta, `ben` on delta and gamma), an approver of delta who asks
/// nothing (`tara`), and an admin; each with a token.
struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    app: Router,
    anna: Person,
    ben: Person,
    tara: Person,
    admin: Person,
}

struct Person {
    id: String,
    token: String,
    public_id: String,
}

const Q_ANNA: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2A";
const Q_BEN_DELTA: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2B";
const Q_BEN_GAMMA: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2C";

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
        admin::grant(
            db,
            email,
            &projects,
            approver,
            &admin::KnownProjects::new(["delta", "gamma"]),
            now,
        )
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

/// A question of `who` on `project` with `status`, asked at `at`, and two of its events.
async fn question(db: &Db, id: &str, who: &Person, project: &str, status: &str, at: DateTime<Utc>) {
    let created = db::time(at);
    sqlx::query(
        "INSERT INTO questions (id, user_id, token_public_id, channel, project, class, status, text_redacted, \
         answer_redacted, created_at) VALUES (?, ?, ?, 'cli', ?, 'progress', ?, 'what was merged?', 'Two items.', ?)",
    )
    .bind(id)
    .bind(&who.id)
    .bind(&who.public_id)
    .bind(project)
    .bind(status)
    .bind(&created)
    .execute(db.pool())
    .await
    .unwrap();
    let qid = QuestionId::parse(id).unwrap();
    let project = ProjectId::parse(project).unwrap();
    let accepted = Event::Accepted(Accepted::new(qid.clone()));
    let _ = audit::record(db, &qid, None, accepted, at).await.unwrap();
    let error = Event::Error(mercury_schema::events::EventError {
        code: ErrorCode::Internal,
        message: "Something failed.".to_owned(),
        cost_usd: mercury_schema::api::Usd::default(),
        judge_mode: None,
    });
    let _ = audit::record(db, &qid, Some(&project), error, at).await.unwrap();
}

async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let anna = person(&db, "anna@example.com", Role::Pm, &["delta"], false, now).await;
    let ben = person(&db, "ben@example.com", Role::Pm, &["delta", "gamma"], false, now).await;
    let tara = person(&db, "tara@example.com", Role::TechLead, &["delta"], true, now).await;
    let admin = person(&db, "admin@example.com", Role::Admin, &[], false, now).await;
    question(&db, Q_ANNA, &anna, "delta", "delivered", now).await;
    question(&db, Q_BEN_DELTA, &ben, "delta", "refused", now).await;
    question(&db, Q_BEN_GAMMA, &ben, "gamma", "delivered", now).await;
    let delta = ProjectId::parse("delta").unwrap();
    audit::action(
        &db,
        "probe_registered",
        Some(&delta),
        &json!({ "access": "code" }),
        now,
    )
    .await
    .unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let app = app(Questions::new(db.clone(), Arc::new(config), Arc::new(NotBuilt)));
    Core {
        _dir: dir,
        db,
        app,
        anna,
        ben,
        tara,
        admin,
    }
}

async fn get(app: &Router, uri: &str, token: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

async fn page(app: &Router, uri: &str, token: &str) -> AuditPage {
    let (status, _, body) = get(app, uri, token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{uri}: {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body).unwrap()
}

/// The rows of a page, as `type` and question.
fn seen(page: &AuditPage) -> Vec<(String, Option<String>)> {
    page.items
        .iter()
        .map(|i| {
            (
                i.event_type.as_str().to_owned(),
                i.question_id.as_ref().map(|q| q.as_str().to_owned()),
            )
        })
        .collect()
}

fn questions_of(page: &AuditPage) -> Vec<&str> {
    let mut questions: Vec<&str> = page
        .items
        .iter()
        .filter_map(|i| i.question_id.as_ref().map(QuestionId::as_str))
        .collect();
    questions.dedup();
    questions
}

// ------------------------------------------------------------------------------------ the tests

/// The audit is an admin's: a project manager, the asker of the questions included, and an
/// approver are refused before a row is read, by the page and by the export alike.
#[tokio::test]
async fn only_an_admin_reads_the_audit() {
    let core = core().await;
    for person in [&core.anna, &core.ben, &core.tara] {
        for uri in ["/v1/audit", "/v1/audit?question=q_01JB2M3N4P5Q6R7S8T9V0W1X2Y"] {
            let (status, _, body) = get(&core.app, uri, &person.token).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "invalid_request");
            assert_eq!(body["error"]["message"], "Only an admin reads the audit.");
            // a terminal prints the hint as the web client does: it names no page to go to
            assert_eq!(
                body["error"]["hint"],
                "What happened to your own question is shown where you asked it; ask an admin for anything else."
            );
        }
        let (status, ..) = get(&core.app, "/v1/audit/export?format=jsonl", &person.token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    let all = page(&core.app, "/v1/audit?limit=500", &core.admin.token).await;
    let last_seq = audit::last_seq(&core.db).await.unwrap();
    assert_eq!(all.items.len() as u64, last_seq, "an admin sees every row");
    assert!(all.next.is_none());

    // the asker's address and token on a question's rows
    let anna = page(
        &core.app,
        &format!("/v1/audit?question={Q_ANNA}"),
        &core.admin.token,
    )
    .await;
    assert_eq!(questions_of(&anna), [Q_ANNA]);
    let row = &anna.items[0];
    assert_eq!(row.user.as_ref().unwrap().as_str(), "anna@example.com");
    assert_eq!(row.token.as_ref().unwrap().as_str(), core.anna.public_id);
}

#[tokio::test]
async fn filters_combine() {
    let core = core().await;
    let app = &core.app;
    let admin = &core.admin.token;

    let by_question = page(app, &format!("/v1/audit?question={Q_BEN_GAMMA}"), admin).await;
    assert_eq!(questions_of(&by_question), [Q_BEN_GAMMA]);

    // a token's rows: its questions, and the admin rows that name it
    let by_token = page(app, &format!("/v1/audit?token={}", core.ben.public_id), admin).await;
    assert_eq!(questions_of(&by_token), [Q_BEN_DELTA, Q_BEN_GAMMA]);
    assert!(seen(&by_token).contains(&("token_created".to_owned(), None)));

    // a person's rows, the address in any case: their questions and what was done to them
    let by_user = page(app, "/v1/audit?user=BEN@example.com", admin).await;
    assert_eq!(questions_of(&by_user), [Q_BEN_DELTA, Q_BEN_GAMMA]);
    let ben_admin_rows: Vec<String> = seen(&by_user)
        .into_iter()
        .filter(|(_, q)| q.is_none())
        .map(|(t, _)| t)
        .collect();
    assert_eq!(ben_admin_rows, ["user_added", "user_granted", "token_created"]);

    let refused = page(app, "/v1/audit?outcome=refused", admin).await;
    assert_eq!(questions_of(&refused), [Q_BEN_DELTA]);
    let gamma_delivered = page(
        app,
        "/v1/audit?project=gamma&outcome=delivered&class=progress",
        admin,
    )
    .await;
    assert_eq!(questions_of(&gamma_delivered), [Q_BEN_GAMMA]);
    assert!(page(app, "/v1/audit?class=health", admin).await.items.is_empty());

    // time: `from` included, `to` not; a date is its midnight UTC
    let tomorrow = (db::now() + TimeDelta::days(1)).format("%Y-%m-%d").to_string();
    assert!(
        page(app, &format!("/v1/audit?from={tomorrow}"), admin)
            .await
            .items
            .is_empty()
    );
    let before_tomorrow = page(app, &format!("/v1/audit?to={tomorrow}&limit=500"), admin).await;
    assert_eq!(
        before_tomorrow.items.len() as u64,
        audit::last_seq(&core.db).await.unwrap()
    );
}

#[tokio::test]
async fn paging_by_the_cursor_never_repeats_or_skips_a_row() {
    let core = core().await;
    let app = &core.app;
    let total = audit::last_seq(&core.db).await.unwrap();
    let mut seqs = Vec::new();
    let mut uri = "/v1/audit?limit=3".to_owned();
    let mut pages = 0;
    loop {
        let page = page(app, &uri, &core.admin.token).await;
        assert!(page.items.len() <= 3);
        seqs.extend(page.items.iter().map(|i| i.seq));
        pages += 1;
        // rows written while paging come after, in later pages
        if pages == 2 {
            audit::action(
                &core.db,
                "tokens_listed",
                None,
                &json!({ "tokens": 4 }),
                db::now(),
            )
            .await
            .unwrap();
        }
        match page.next {
            Some(next) => uri = format!("/v1/audit?limit=3&cursor={next}"),
            None => break,
        }
    }
    let expected: Vec<u64> = (1..=total + 1).collect();
    assert_eq!(seqs, expected, "every row once, in seq order");
}

#[tokio::test]
async fn a_parameter_that_does_not_parse_is_refused_by_its_name() {
    let core = core().await;
    let token = &core.admin.token;
    for (query, name) in [
        ("limit=0", "limit"),
        ("limit=501", "limit"),
        ("limit=ten", "limit"),
        ("cursor=abc", "cursor"),
        ("from=yesterday", "from"),
        ("to=2026-13-01", "to"),
        ("class=gossip", "class"),
        ("outcome=maybe", "outcome"),
        ("project=Delta", "project"),
        ("user=anna", "user"),
        ("token=short", "token"),
        ("question=q_1", "question"),
        ("format=jsonl", "format"),
    ] {
        let (status, _, body) = get(&core.app, &format!("/v1/audit?{query}"), token).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "invalid_request", "{query}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(&format!("`{name}`")), "{query}: {message}");
    }
    let (status, _, _) = get(&core.app, "/v1/audit?colour=blue", token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an unknown parameter");
    let (status, _, _) = get(&core.app, "/v1/audit", "mcy_nobody00000_secret").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A row's hash, recomputed from the export's own fields as the data model writes it out.
fn recompute(row: &Value) -> String {
    let bytes = |field: &str| -> Vec<u8> {
        let hex = row[field].as_str().unwrap();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    };
    let event_type = row["type"].as_str().unwrap();
    let mut hasher = Sha256::new();
    hasher.update(bytes("prev_hash"));
    hasher.update(row["seq"].as_u64().unwrap().to_be_bytes());
    hasher.update([u8::try_from(event_type.len()).unwrap()]);
    hasher.update(event_type.as_bytes());
    hasher.update(bytes("payload_sha256"));
    // `at` as the file writes it: the stored time, milliseconds included
    hasher.update(row["at"].as_str().unwrap().as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks an export that starts at the first row from the file alone: each row follows the one
/// before, its payload's bytes as the file holds them give its digest, and its hash recomputes.
/// Gives the rows.
fn verify_file(file: &str) -> Vec<Value> {
    let mut prev = "0".repeat(64);
    let mut rows = Vec::new();
    for line in file.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            row["prev_hash"],
            prev.as_str(),
            "row {} follows the one before",
            row["seq"]
        );
        assert_eq!(
            row["hash"],
            recompute(&row).as_str(),
            "row {} recomputes",
            row["seq"]
        );
        // the payload's bytes as the file holds them, never written out again
        let start = line.find(r#""payload":"#).unwrap() + r#""payload":"#.len();
        let payload = &line[start..line.find(r#","payload_sha256":"#).unwrap()];
        let digest: String = Sha256::digest(payload.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            row["payload_sha256"],
            digest.as_str(),
            "row {} payload",
            row["seq"]
        );
        prev = row["hash"].as_str().unwrap().to_owned();
        rows.push(row);
    }
    rows
}

#[tokio::test]
async fn the_export_is_an_admins_and_its_file_verifies_without_the_database() {
    let core = core().await;
    let app = &core.app;
    let (status, _, body) = get(app, "/v1/audit/export?format=jsonl", &core.tara.token).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    for query in ["format=csv", "", "format=jsonl&limit=10", "format=jsonl&cursor=4"] {
        let (status, _, _) = get(app, &format!("/v1/audit/export?{query}"), &core.admin.token).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }

    let upto = audit::last_seq(&core.db).await.unwrap();
    let (status, headers, body) = get(app, "/v1/audit/export?format=jsonl", &core.admin.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_TYPE], "application/jsonl");
    assert!(
        headers[CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    let rows = verify_file(std::str::from_utf8(&body).unwrap());
    assert_eq!(
        rows.len() as u64,
        upto,
        "every row up to the start, not the export's own"
    );

    // the export is itself a row, after what it covered, with its filters and its caller
    let one = format!("/v1/audit/export?format=jsonl&question={Q_ANNA}");
    let (_, _, body) = get(app, &one, &core.admin.token).await;
    let lines: Vec<Value> = std::str::from_utf8(&body)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "one question's two rows");
    assert!(lines.iter().all(|r| r["question_id"] == Q_ANNA));
    let last = page(app, &format!("/v1/audit?cursor={}", upto + 1), &core.admin.token).await;
    let exported: Vec<&Value> = last.items.iter().filter_map(|i| i.payload.as_ref()).collect();
    assert_eq!(last.items[0].event_type.as_str(), "audit_exported");
    assert_eq!(
        exported[0],
        &json!({ "by": "api", "token": core.admin.public_id, "filters": { "question": Q_ANNA }, "upto": upto + 1 })
    );
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

/// The API's export writes each row as stored, as `admin audit export` does: a row on a whole
/// second keeps its `.000`, and a payload keeps its own bytes, keys in an order of their own and a
/// float written with more digits than it needs, so every line re-digests from the file alone,
/// byte for byte. Both exports write the same lines.
#[tokio::test]
async fn the_apis_export_writes_each_row_as_stored() {
    let core = core().await;
    // a row as another writer may have stored it, chained after the last one
    let stored = r#"{"source":"explicit","p":0.1000000000000000055511151231257827,"project":"delta"}"#;
    let at = "2026-10-02T10:00:01.000Z";
    let last = audit::last_seq(&core.db).await.unwrap();
    let prev: Vec<u8> = sqlx::query_scalar("SELECT hash FROM audit_events WHERE seq = ?")
        .bind(i64::try_from(last).unwrap())
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    let payload_sha256: [u8; 32] = Sha256::digest(stored.as_bytes()).into();
    let hash = audit::chain_hash(
        &prev.clone().try_into().unwrap(),
        last + 1,
        "project",
        &payload_sha256,
        at,
    );
    sqlx::query(
        "INSERT INTO audit_events (question_id, type, payload_json, payload_sha256, prev_hash, hash, created_at) \
         VALUES (?, 'project', ?, ?, ?, ?, ?)",
    )
    .bind(Q_ANNA)
    .bind(stored)
    .bind(payload_sha256.to_vec())
    .bind(prev)
    .bind(hash.to_vec())
    .bind(at)
    .execute(core.db.pool())
    .await
    .unwrap();
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);

    let (status, _, body) = get(&core.app, "/v1/audit/export?format=jsonl", &core.admin.token).await;
    assert_eq!(status, StatusCode::OK);
    let api = String::from_utf8(body).unwrap();
    let rows = verify_file(&api);
    assert_eq!(rows.len() as u64, last + 1);
    assert!(api.contains(&format!(r#""at":"{at}""#)), "{api}");
    assert!(api.contains(stored), "{api}");

    // the admin command's file: the same lines, then the API export's own row
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let command = Cli::try_parse_from(["mercury-core", "admin", "audit", "export"]).unwrap();
    let Some(Command::Admin(command)) = command.command else {
        panic!("not an admin command");
    };
    let (mut out, mut note) = (Vec::new(), Vec::new());
    admin::run(command, &core.db, &examples, &mut out, &mut note)
        .await
        .unwrap();
    let admins = String::from_utf8(out).unwrap();
    assert!(admins.starts_with(&api), "the API's lines first: {admins}");
    let all = verify_file(&admins);
    assert_eq!(all.len(), rows.len() + 1);
    assert_eq!(all[rows.len()]["type"], "audit_exported");
}

/// An export and an access listing are an admin's own acts: their rows carry no project, so an
/// approver of the project they name pages that project and finds neither. What they asked for
/// stays in the payload.
#[tokio::test]
async fn an_export_and_an_access_listing_of_a_project_are_not_rows_of_that_project() {
    let core = core().await;
    let app = &core.app;
    let before = audit::last_seq(&core.db).await.unwrap();

    // the API's export, then the admin commands' export and access listing, each naming delta
    let (status, ..) = get(
        app,
        "/v1/audit/export?format=jsonl&project=delta",
        &core.admin.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    for args in [
        ["audit", "export", "--project", "delta"],
        ["access", "list", "--project", "delta"],
    ] {
        let command = Cli::try_parse_from(["mercury-core", "admin"].iter().chain(args.iter())).unwrap();
        let Some(Command::Admin(command)) = command.command else {
            panic!("not an admin command");
        };
        let (mut out, mut note) = (Vec::new(), Vec::new());
        admin::run(command, &core.db, &examples, &mut out, &mut note)
            .await
            .unwrap();
    }

    // the admin sees all three rows, with no project, and what they asked for in the payload
    let admins = page(app, &format!("/v1/audit?cursor={before}"), &core.admin.token).await;
    let types: Vec<&str> = admins.items.iter().map(|i| i.event_type.as_str()).collect();
    assert_eq!(types, ["audit_exported", "audit_exported", "access_listed"]);
    assert!(admins.items.iter().all(|i| i.project.is_none()));
    let payloads: Vec<&Value> = admins.items.iter().filter_map(|i| i.payload.as_ref()).collect();
    assert_eq!(payloads[0]["filters"], json!({ "project": "delta" }));
    assert_eq!(payloads[1]["filters"], json!({ "project": "delta" }));
    assert_eq!(payloads[2]["project"], "delta");

    // the approver of delta reads none of it: the audit is an admin's
    let (status, ..) = get(app, "/v1/audit?project=delta", &core.tara.token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

#[tokio::test]
async fn retention_clears_old_payloads_and_texts_and_keeps_the_chain() {
    let core = core().await;
    let now = db::now();
    let old = now - TimeDelta::days(100);
    question(
        &core.db,
        "q_01JB2M3N4P5Q6R7S8T9V0W1X2D",
        &core.anna,
        "delta",
        "delivered",
        old,
    )
    .await;
    // the answer held for an approver, never delivered: its flagged sentence and a note on it
    let reason = serde_json::json!({
        "reason": "review_band",
        "flags": [{ "sentence": "The held sentence.", "check": "sentence_contains_code_or_identifier", "p": 0.45 }],
        "composer": "model"
    });
    sqlx::query(
        "INSERT INTO approvals (id, question_id, reason_json, draft_answer, status, note, created_at, expires_at)
         VALUES ('ap_01JB2M3N4P5Q6R7S8T9V0W1X2D', 'q_01JB2M3N4P5Q6R7S8T9V0W1X2D', ?, '{\"text\":\"held\"}',
                 'rejected', 'Ask the lead first.', ?, ?)",
    )
    .bind(reason.to_string())
    .bind(db::time(old))
    .bind(db::time(old + TimeDelta::days(1)))
    .execute(core.db.pool())
    .await
    .unwrap();
    let before = audit::last_seq(&core.db).await.unwrap();

    let retained = audit::retain(&core.db, 90, now).await.unwrap();
    assert_eq!(
        retained,
        Retained {
            payloads: 2,
            questions: 1,
            approvals: 1
        }
    );
    assert_eq!(
        audit::verify(&core.db).await.unwrap().broken,
        None,
        "the chain still holds"
    );

    let (text, answer): (String, Option<String>) = sqlx::query_as(
        "SELECT text_redacted, answer_redacted FROM questions WHERE id = 'q_01JB2M3N4P5Q6R7S8T9V0W1X2D'",
    )
    .fetch_one(core.db.pool())
    .await
    .unwrap();
    assert_eq!((text.as_str(), answer), ("", None));
    let (draft, note, reason): (String, Option<String>, String) = sqlx::query_as(
        "SELECT draft_answer, note, reason_json FROM approvals WHERE id = 'ap_01JB2M3N4P5Q6R7S8T9V0W1X2D'",
    )
    .fetch_one(core.db.pool())
    .await
    .unwrap();
    assert_eq!(
        (draft.as_str(), note),
        ("", None),
        "the held draft and its note go with the answer"
    );
    let reason: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(
        reason["flags"],
        serde_json::json!([{ "sentence": "", "check": "sentence_contains_code_or_identifier", "p": 0.45 }]),
        "the held sentences go too; the check and its p stay"
    );
    assert_eq!(reason["reason"], "review_band");
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM questions WHERE text_redacted = 'what was merged?'")
            .fetch_one(core.db.pool())
            .await
            .unwrap();
    assert_eq!(kept, 3, "the recent questions keep their text");

    // the old rows are still there, without their payloads, with their hashes
    let rows = page(
        &core.app,
        "/v1/audit?question=q_01JB2M3N4P5Q6R7S8T9V0W1X2D",
        &core.admin.token,
    )
    .await;
    assert_eq!(rows.items.len(), 2);
    assert!(rows.items.iter().all(|r| r.payload.is_none()));
    let last = page(
        &core.app,
        &format!("/v1/audit?cursor={before}"),
        &core.admin.token,
    )
    .await;
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.items[0].event_type.as_str(), "audit_retained");
    let payload = last.items[0].payload.as_ref().unwrap();
    assert_eq!(
        (
            payload["days"].as_u64(),
            payload["payloads"].as_u64(),
            payload["questions"].as_u64(),
            payload["approvals"].as_u64()
        ),
        (Some(90), Some(2), Some(1), Some(1))
    );

    // a second run finds nothing and writes nothing
    assert_eq!(
        audit::retain(&core.db, 90, now).await.unwrap(),
        Retained {
            payloads: 0,
            questions: 0,
            approvals: 0
        }
    );
    assert_eq!(audit::last_seq(&core.db).await.unwrap(), before + 1);
}

/// A question of `who` that ended with `ending`: its row, its `accepted` and the ending.
async fn ended(db: &Db, id: &str, who: &Person, ending: Event, status: &str, at: DateTime<Utc>) {
    let created = db::time(at);
    sqlx::query(
        "INSERT INTO questions (id, user_id, token_public_id, channel, project, class, status, text_redacted, \
         created_at) VALUES (?, ?, ?, 'cli', 'delta', 'progress', ?, 'what was merged?', ?)",
    )
    .bind(id)
    .bind(&who.id)
    .bind(&who.public_id)
    .bind(status)
    .bind(&created)
    .execute(db.pool())
    .await
    .unwrap();
    let qid = QuestionId::parse(id).unwrap();
    let delta = ProjectId::parse("delta").unwrap();
    let accepted = Event::Accepted(Accepted {
        id: qid.clone(),
        follows: None,
    });
    let _ = audit::record(db, &qid, None, accepted, at).await.unwrap();
    let _ = audit::record(db, &qid, Some(&delta), ending, at).await.unwrap();
}

/// Once a token is revoked, `audit export --token <id>` lists every question it asked with how each
/// ended, the token's creation and its revocation, and nothing of anyone else; the file holds no
/// secret, and the export is itself a row.
#[tokio::test]
async fn a_revoked_tokens_export_lists_its_questions_with_their_outcomes() {
    let core = core().await;
    let now = db::now();
    let carl = person(&core.db, "carl@example.com", Role::Pm, &["delta"], false, now).await;
    let usd = mercury_schema::api::Usd::default();
    let (delivered, refused, failed) = (
        "q_01JB2M3N4P5Q6R7S8T9V0W3A3A",
        "q_01JB2M3N4P5Q6R7S8T9V0W3A3B",
        "q_01JB2M3N4P5Q6R7S8T9V0W3A3C",
    );
    ended(
        &core.db,
        delivered,
        &carl,
        Event::Done(mercury_schema::events::Done {
            duration_ms: 1200,
            cost_usd: usd,
            judge_mode: mercury_schema::JudgeMode::Jev,
            composer: mercury_schema::events::ComposerMode::Model,
            audit_id: 1,
        }),
        "delivered",
        now,
    )
    .await;
    ended(
        &core.db,
        refused,
        &carl,
        Event::Refused(mercury_schema::events::Refused::new(
            mercury_schema::api::Refusal {
                code: mercury_schema::api::RefusalCode::AsksForCode,
                message: "Mercury doesn't show source code, file names or paths.".to_owned(),
                hint: "Ask what is done instead.".to_owned(),
            },
            usd,
            Some(mercury_schema::JudgeMode::Jev),
        )),
        "refused",
        now,
    )
    .await;
    ended(
        &core.db,
        failed,
        &carl,
        Event::Error(mercury_schema::events::EventError {
            code: ErrorCode::ProbeUnavailable,
            message: "The project's data isn't answering.".to_owned(),
            cost_usd: usd,
            judge_mode: None,
        }),
        "failed",
        now,
    )
    .await;

    // the token is revoked while in use: it asks nothing more, and what it asked stays
    admin::revoke_token(&core.db, &carl.public_id, db::now())
        .await
        .unwrap();
    let (status, ..) = get(&core.app, "/v1/projects", &carl.token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked token is refused");

    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let command = Cli::try_parse_from([
        "mercury-core",
        "admin",
        "audit",
        "export",
        "--token",
        &carl.public_id,
    ])
    .unwrap();
    let Some(Command::Admin(command)) = command.command else {
        panic!("not an admin command");
    };
    let (mut out, mut note) = (Vec::new(), Vec::new());
    admin::run(command, &core.db, &examples, &mut out, &mut note)
        .await
        .unwrap();
    let file = String::from_utf8(out).unwrap();
    let rows: Vec<Value> = file
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    // every question of the token, each with its ending, in the order they were written
    let endings: Vec<(&str, &str)> = rows
        .iter()
        .filter_map(|row| Some((row["question_id"].as_str()?, row["type"].as_str()?)))
        .filter(|(_, kind)| ["done", "refused", "error"].contains(kind))
        .collect();
    assert_eq!(
        endings,
        [(delivered, "done"), (refused, "refused"), (failed, "error")],
        "{file}"
    );
    let accepted = rows.iter().filter(|row| row["type"] == "accepted").count();
    assert_eq!(accepted, 3, "each question's start");
    let kinds: Vec<&str> = rows.iter().filter_map(|row| row["type"].as_str()).collect();
    assert!(
        kinds.contains(&"token_created") && kinds.contains(&"token_revoked"),
        "{kinds:?}"
    );
    // nothing of anyone else's, and no secret
    assert!(
        rows.iter()
            .filter_map(|row| row["question_id"].as_str())
            .all(|id| [delivered, refused, failed].contains(&id)),
        "{file}"
    );
    assert!(!file.contains(&carl.token), "the file holds no token");
    assert!(!file.contains(Q_ANNA) && !file.contains(Q_BEN_DELTA));

    // each row carries its hashes, and the export is itself a row after them
    assert!(
        rows.iter()
            .all(|row| row["hash"].is_string() && row["prev_hash"].is_string())
    );
    let last = audit::last_seq(&core.db).await.unwrap();
    let written = page(
        &core.app,
        &format!("/v1/audit?cursor={}", last - 1),
        &core.admin.token,
    )
    .await;
    assert_eq!(
        written.items.last().unwrap().event_type.as_str(),
        "audit_exported"
    );
}
