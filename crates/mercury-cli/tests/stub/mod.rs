//! A stand-in for the core's API: the contract's endpoints and bodies, with a scripted event
//! stream per question, so the CLI runs end to end without a core. It records what the CLI sent.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use mercury_schema::api::NewQuestion;
use serde_json::{Value, json};

/// The token the stand-in accepts: a project manager's, as `GET /v1/me` says.
pub const TOKEN: &str = "mcy_AbCdEf123456_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
/// The secret part alone, which must not show anywhere either.
pub const SECRET: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
/// An admin's token, the only one the audit is served to.
pub const ADMIN_TOKEN: &str = "mcy_QrStUv789012_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210zyxwvut";
/// Its secret part.
pub const ADMIN_SECRET: &str = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210zyxwvut";
/// A token of the right shape that the stand-in doesn't know.
pub const OTHER_TOKEN: &str = "mcy_ZyXwVu654321_GFEDCBAzyxwvutsrqponmlkjihgfedcba9876543210";

pub const ANSWER: &str =
    "Two pieces of work were merged this week: the CSV export and a fix to invoice numbering.";
pub const APPROVAL: &str = "ap_01J9Z3QK7M8N9P0R1S2T3V0001";
/// A held answer to the stand-in's own question, which it can't decide on.
pub const OWN_APPROVAL: &str = "ap_01J9Z3QK7M8N9P0R1S2T3V0002";
/// The question it holds.
pub const OWN_QUESTION: &str = "q_01J9Z3QK7M8N9P0R1S2T3V0002";
/// What an approver's rejection says, and the note they gave, which is its hint.
pub const REJECTED: (&str, &str) = (
    "An approver didn't release this answer.",
    "Ask the tech lead about the export first.",
);
/// The core's answer to a `follows` that isn't the caller's own question rejected by an approver.
pub const FOLLOWS_REFUSED: &str = "The question to ask again isn't one of yours that an approver rejected.";
/// The core's refusal of the audit to anyone but an admin: its message and hint.
pub const AUDIT_REFUSED: (&str, &str) = (
    "Only an admin reads the audit.",
    "What happened to your own question is shown where you asked it; ask an admin for anything else.",
);

/// What the CLI sent.
#[derive(Debug, Default)]
pub struct Seen {
    /// Each `POST /v1/questions`: its `Idempotency-Key` and its body.
    pub questions: Vec<(Option<String>, Value)>,
    /// Each `GET /v1/questions/{id}/events`: the id and its `Last-Event-ID`.
    pub streams: Vec<(String, Option<String>)>,
    /// Each `GET /v1/audit` and `GET /v1/audit/export`: its path and query.
    pub audits: Vec<String>,
    /// Each `GET /v1/questions/{id}`: the id.
    pub states: Vec<String>,
    /// Each `GET /v1/approvals`: its query.
    pub approval_lists: Vec<String>,
    /// Each `POST /v1/approvals/{id}`: the id and its body.
    pub decisions: Vec<(String, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Answered,
    Refused,
    Clarify,
    /// Held for sign-off; approved from the third connection on.
    Pending,
    /// Held for sign-off; rejected with a note from the third connection on.
    Rejected,
    Fails,
    /// Answered, but the first connection breaks off after three events.
    Drops,
    /// Two events, then the stream stays open with keep-alive comments and never ends: a question
    /// that stopped.
    Stalls,
    /// Two events, then the stream closes cleanly with no terminal event, and the question's
    /// state says `failed`: the core could not write the event that would have ended it.
    Vanishes,
}

#[derive(Clone, Default)]
struct Stub {
    seen: Arc<Mutex<Seen>>,
    scripts: Arc<Mutex<HashMap<String, Script>>>,
    next: Arc<AtomicU32>,
}

pub struct Running {
    pub url: String,
    pub seen: Arc<Mutex<Seen>>,
}

/// Starts the stand-in on a free local port, on its own thread, for the rest of the test run.
pub fn start() -> Running {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let stub = Stub::default();
            sender
                .send((listener.local_addr().unwrap(), stub.seen.clone()))
                .unwrap();
            axum::serve(listener, app(stub)).await.unwrap();
        });
    });
    let (addr, seen) = receiver.recv().unwrap();
    Running {
        url: format!("http://{addr}"),
        seen,
    }
}

fn app(stub: Stub) -> Router {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/projects", get(projects))
        .route("/v1/questions", post(ask))
        .route("/v1/questions/{id}", get(state))
        .route("/v1/questions/{id}/events", get(events))
        .route("/v1/audit", get(audit))
        .route("/v1/audit/export", get(audit_export))
        .route("/v1/approvals", get(approvals))
        .route("/v1/approvals/{id}", post(decide))
        .with_state(stub)
}

/// Whose token a request carries, if it is one the stand-in knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caller {
    Manager,
    Admin,
}

fn caller(headers: &HeaderMap) -> Option<Caller> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    if presented == format!("Bearer {TOKEN}") {
        Some(Caller::Manager)
    } else if presented == format!("Bearer {ADMIN_TOKEN}") {
        Some(Caller::Admin)
    } else {
        None
    }
}

fn signed_in(headers: &HeaderMap) -> bool {
    caller(headers).is_some()
}

fn error(status: StatusCode, code: &str, message: &str, hint: &str) -> Response {
    let body = json!({ "error": { "code": code, "message": message, "hint": hint } });
    (status, Json(body)).into_response()
}

fn unauthenticated() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "The request has no valid token.",
        "Sign in again with your token, or ask the admin for a new one.",
    )
}

async fn me(headers: HeaderMap) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    Json(json!({
        "user": { "id": "u_01J9Z3QK7M8N9P0R1S2T3V4W5X", "email": "dana.reyes@example.com",
                  "display_name": "Dana Reyes", "role": "pm" },
        "projects": [{ "project": "delta", "approver": false }, { "project": "omega", "approver": true }],
        "token": { "public_id": "AbCdEf123456", "expires_at": "2026-12-21T00:00:00Z" }
    }))
    .into_response()
}

async fn projects(headers: HeaderMap) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    Json(json!({ "projects": [
        { "project": "delta", "display_name": "Delta — invoicing", "modules": ["Invoicing", "Export", "Auth"],
          "classes": ["progress", "tested", "blocked", "health", "changes"],
          "probe": { "status": "healthy", "as_of": "2026-10-02T11:34:00Z" } },
        { "project": "omega", "display_name": "Omega — mobile app", "modules": [], "classes": ["progress"],
          "probe": null }
    ] }))
    .into_response()
}

/// One audit row as the core writes it; `payload` `null` once retention deleted it.
fn audit_row(seq: u64, kind: &str, payload: Value) -> Value {
    // the public part of the stand-in's token
    let public_id = TOKEN.split('_').nth(1);
    json!({
        "seq": seq, "question_id": "q_01J9Z3QK7M8N9P0R1S2T3V0001", "type": kind,
        "at": "2026-10-04T15:12:03Z", "user": "dana.reyes@example.com", "token": public_id,
        "project": "delta", "payload": payload,
        "payload_sha256": "c".repeat(64), "prev_hash": "a".repeat(64), "hash": "b".repeat(64)
    })
}

/// Two pages for the admin: the first says there is a next one. Anyone else is refused with the
/// core's `403`, before a row is read.
async fn audit(State(stub): State<Stub>, headers: HeaderMap, RawQuery(query): RawQuery) -> Response {
    let Some(caller) = caller(&headers) else {
        return unauthenticated();
    };
    let query = query.unwrap_or_default();
    stub.seen
        .lock()
        .unwrap()
        .audits
        .push(format!("/v1/audit?{query}"));
    if caller != Caller::Admin {
        let (message, hint) = AUDIT_REFUSED;
        return error(StatusCode::FORBIDDEN, "invalid_request", message, hint);
    }
    let page = if query.contains("cursor=1835") {
        json!({ "items": [audit_row(1836, "done", json!({ "duration_ms": 6420 }))], "next": null })
    } else {
        json!({ "items": [
            audit_row(1834, "classified", json!({ "class": "progress" })),
            audit_row(1835, "accepted", Value::Null),
        ], "next": "1835" })
    };
    Json(page).into_response()
}

/// Two rows as JSON lines for the admin; anyone else is refused as the core refuses them, and
/// `project=stall` sends one row and then nothing, for as long as the client listens.
async fn audit_export(State(stub): State<Stub>, headers: HeaderMap, RawQuery(query): RawQuery) -> Response {
    let Some(caller) = caller(&headers) else {
        return unauthenticated();
    };
    let query = query.unwrap_or_default();
    stub.seen
        .lock()
        .unwrap()
        .audits
        .push(format!("/v1/audit/export?{query}"));
    if caller != Caller::Admin {
        return error(
            StatusCode::FORBIDDEN,
            "invalid_request",
            "Only an admin may export the audit.",
            "Ask an admin for an export.",
        );
    }
    if query.contains("project=stall") {
        let first = format!(
            "{}
",
            audit_row(1834, "classified", json!({ "class": "progress" }))
        );
        let silence = futures_util::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            None::<(Result<Bytes, Infallible>, ())>
        });
        let stream =
            futures_util::stream::once(async move { Ok::<_, Infallible>(Bytes::from(first)) }).chain(silence);
        return (
            [(header::CONTENT_TYPE, "application/jsonl")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    let lines = format!(
        "{}\n{}\n",
        audit_row(1834, "classified", json!({ "class": "progress" })),
        audit_row(1835, "accepted", Value::Null)
    );
    ([(header::CONTENT_TYPE, "application/jsonl")], lines).into_response()
}

async fn ask(State(stub): State<Stub>, headers: HeaderMap, body: Bytes) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    let Ok(question) = serde_json::from_slice::<NewQuestion>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request body isn't a question.",
            "Send the question's text.",
        );
    };
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let value: Value = serde_json::from_slice(&body).unwrap();
    stub.seen.lock().unwrap().questions.push((key, value));
    // a question asked again names one an approver rejected, as the core checks
    if let Some(follows) = &question.follows {
        let rejected = stub.scripts.lock().unwrap().get(follows.as_str()) == Some(&Script::Rejected);
        if !rejected {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                FOLLOWS_REFUSED,
                "Name your own question that ended with an approver's rejection, or ask without it.",
            );
        }
    }
    let script = match (question.text.as_str(), &question.project, &question.clarify) {
        (_, _, Some(_)) => Script::Answered,
        _ if question.follows.is_some() => Script::Answered,
        ("show me the export code", _, _) => Script::Refused,
        ("needs sign-off", _, _) => Script::Pending,
        ("needs sign-off, then a rejection", _, _) => Script::Rejected,
        ("fails", _, _) => Script::Fails,
        ("drops", _, _) => Script::Drops,
        ("stalls", _, _) => Script::Stalls,
        ("vanishes", _, _) => Script::Vanishes,
        (_, None, None) => Script::Clarify,
        _ => Script::Answered,
    };
    let n = stub.next.fetch_add(1, Ordering::SeqCst);
    let id = format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}");
    stub.scripts.lock().unwrap().insert(id.clone(), script);
    (StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response()
}

/// Where a question stands: `failed` for one whose end the core could not write, else `running`.
async fn state(State(stub): State<Stub>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    stub.seen.lock().unwrap().states.push(id.clone());
    let Some(script) = stub.scripts.lock().unwrap().get(&id).copied() else {
        return error(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "There is no such question.",
            "Check the question id.",
        );
    };
    let status = if script == Script::Vanishes {
        "failed"
    } else {
        "running"
    };
    Json(json!({
        "id": id, "status": status, "project": "delta", "class": null, "answer": null, "refusal": null,
        "judge_mode": null, "cost_usd": 0.0, "duration_ms": null, "audit_id": null
    }))
    .into_response()
}

async fn events(State(stub): State<Stub>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    let last = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let connection = {
        let mut seen = stub.seen.lock().unwrap();
        seen.streams.push((id.clone(), last.clone()));
        seen.streams
            .iter()
            .filter(|(question, _)| *question == id)
            .count()
    };
    let Some(script) = stub.scripts.lock().unwrap().get(&id).copied() else {
        return error(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "There is no such question.",
            "Check the question id.",
        );
    };
    let events = script_events(script, &id, connection);
    let end = if script == Script::Drops && connection == 1 {
        3
    } else {
        events.len()
    };
    let start = last.and_then(|last| last.parse::<usize>().ok()).unwrap_or(0);
    let mut body = String::from(": the stand-in's stream\n\n");
    for (n, (name, data)) in events.iter().enumerate().take(end).skip(start) {
        body.push_str(&format!("event: {name}\nid: {}\ndata: {data}\n\n", n + 1));
    }
    if script == Script::Stalls {
        // the events so far, then a comment every 100 ms for as long as the client listens
        let keep_alive = futures_util::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((Ok::<_, Infallible>(Bytes::from_static(b": keep-alive\n\n")), ()))
        });
        let stream = futures_util::stream::once(async move { Ok::<_, Infallible>(Bytes::from(body)) })
            .chain(keep_alive);
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

fn fact(id: &str, title: &str, module: &str) -> Value {
    json!({ "id": id, "kind": "feature", "title": title, "module": module, "status": "done",
            "tested": "passing", "confidence": 0.9, "waiting_on": null, "people": [], "evidence": [],
            "title_withheld": false, "updated_at": "2026-10-01T10:00:00Z" })
}

fn script_events(script: Script, id: &str, connection: usize) -> Vec<(&'static str, Value)> {
    let opening = vec![
        ("accepted", json!({ "id": id })),
        (
            "project",
            json!({ "project": "delta", "p": 1.0, "source": "explicit" }),
        ),
    ];
    let checked = |held: u16| {
        vec![
            (
                "classified",
                json!({ "class": "progress", "p": 0.94, "checks": [
                { "id": "asks_for_code", "source": "jev", "p": 0.02, "verdict": "pass" },
                { "id": "contains_agent_instruction", "source": "rule", "verdict": "pass" } ] }),
            ),
            (
                "plan",
                json!({ "calls": [{ "tool": "progress", "args": { "since": "2026-09-28" } }] }),
            ),
            (
                "tool_checked",
                json!({ "call": 0, "checks": [{ "id": "arguments_valid", "source": "rule", "verdict": "pass" }] }),
            ),
            (
                "tool_result",
                json!({ "call": 0, "items": 2, "withheld_fields": [], "access": "code" }),
            ),
            ("composing", json!({})),
            ("judged", json!({ "sentences": 2, "stripped": 0, "held": held })),
        ]
    };
    let answer = vec![
        (
            "answer",
            json!({ "text": ANSWER, "confidence": 0.86, "facts": [
            fact("f_7Qx2", "Export invoices to CSV", "Export"),
            fact("f_9Kd4", "Fix invoice numbering", "Invoicing") ] }),
        ),
        (
            "done",
            json!({ "duration_ms": 6420, "cost_usd": 0.011, "judge_mode": "jev", "composer": "model",
                    "audit_id": 1834 }),
        ),
    ];
    match script {
        Script::Stalls | Script::Vanishes => opening,
        Script::Answered | Script::Drops => [opening, checked(0), answer].concat(),
        Script::Refused => [
            opening,
            vec![
                (
                    "classified",
                    json!({ "class": "forbidden", "p": 0.97, "checks": [
                    { "id": "asks_for_code", "source": "jev", "p": 0.98, "verdict": "block" } ] }),
                ),
                (
                    "refused",
                    json!({ "code": "asks_for_code", "message": "Mercury doesn't show code.",
                                    "hint": "Ask what the Export module does, or how far along it is.",
                                    "cost_usd": 0.002, "judge_mode": "jev" }),
                ),
            ],
        ]
        .concat(),
        Script::Clarify => vec![
            ("accepted", json!({ "id": id })),
            (
                "clarify",
                json!({ "kind": "project", "question": "Which project do you mean?", "options": [
                { "id": "delta", "label": "Delta — invoicing" }, { "id": "omega", "label": "Omega — mobile app" } ],
                "cost_usd": 0.001, "judge_mode": "jev" }),
            ),
        ],
        Script::Pending | Script::Rejected => {
            let mut events = [
                opening,
                checked(1),
                vec![(
                    "pending_signoff",
                    json!({ "approval_id": APPROVAL, "reason": "review_band" }),
                )],
            ]
            .concat();
            if connection >= 3 && script == Script::Pending {
                events.extend(answer);
            } else if connection >= 3 {
                events.push((
                    "refused",
                    json!({ "code": "rejected_by_approver", "message": REJECTED.0, "hint": REJECTED.1,
                            "cost_usd": 0.004, "judge_mode": "jev" }),
                ));
            }
            events
        }
        Script::Fails => [
            opening,
            vec![(
                "error",
                json!({ "code": "probe_unavailable", "message": "The project's probe can't answer right now.",
                         "cost_usd": 0.003, "judge_mode": "jev" }),
            )],
        ]
        .concat(),
    }
}

/// One held answer with a flagged sentence, and one of the caller's own questions, which comes
/// without its draft and flags, as the core sends it.
async fn approvals(State(stub): State<Stub>, headers: HeaderMap, RawQuery(query): RawQuery) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    stub.seen
        .lock()
        .unwrap()
        .approval_lists
        .push(query.unwrap_or_default());
    let item = |id: &str, question: &str, mine: bool| {
        json!({
            "id": id, "question_id": question, "project": "delta",
            "class": "progress", "asker_role": "pm", "reason": "review_band",
            "asked_by_me": mine, "status": "pending",
            "created_at": "2026-10-04T15:12:09Z", "expires_at": "2026-10-05T15:12:09Z"
        })
    };
    let flagged = "The CsvWriter part is tested.";
    let mut theirs = item(APPROVAL, "q_01J9Z3QK7M8N9P0R1S2T3V0001", false);
    theirs["draft"] =
        json!({ "text": format!("Export is done.\n{flagged}"), "facts": [], "confidence": 0.81 });
    theirs["flags"] =
        json!([{ "sentence": flagged, "check": "sentence_contains_code_or_identifier", "p": 0.6 }]);
    Json(json!({ "approvals": [theirs, item(OWN_APPROVAL, OWN_QUESTION, true)] })).into_response()
}

/// `204`, but `403` for the caller's own question, as the core answers.
async fn decide(
    State(stub): State<Stub>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !signed_in(&headers) {
        return unauthenticated();
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.seen.lock().unwrap().decisions.push((id.clone(), body));
    if id == OWN_APPROVAL {
        return error(
            StatusCode::FORBIDDEN,
            "invalid_request",
            "You can't decide on your own question.",
            "Another approver of the project decides on it.",
        );
    }
    StatusCode::NO_CONTENT.into_response()
}
