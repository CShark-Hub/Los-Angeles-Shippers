//! Snapshots of the core's API bodies and of a question's stream. Each example is read into its
//! type and written back, so the snapshot shows what the type accepts and what it sends. A
//! snapshot that changes is a contract that changes: review it as one.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use insta::assert_json_snapshot;
use mercury_schema::Event;
use mercury_schema::admin::{AdminOverview, AdminPeople, AdminRepositories};
use mercury_schema::api::{
    Accepted, ApprovalDecision, ApprovalList, AuditPage, ErrorBody, Feedback, Me, NewQuestion,
    ProbeRegistration, ProjectList, QuestionState, ServiceHealth, SessionRequest,
};
use mercury_schema::probe_judge::{JudgeReply, JudgeRequest};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Reads `value` into `T` and writes it back.
fn through<T: DeserializeOwned + Serialize>(value: Value) -> Value {
    let typed: T = serde_json::from_value(value).unwrap();
    serde_json::to_value(typed).unwrap()
}

fn fact() -> Value {
    json!({
        "id": "f_7Qx2", "kind": "feature", "title": "Invoice list with filters", "module": "Invoicing",
        "status": "done", "tested": "passing", "confidence": 0.93, "waiting_on": null, "people": [],
        "evidence": [{ "type": "pr", "ref": "#412", "state": "merged", "at": "2026-10-04" }],
        "title_withheld": false, "updated_at": "2026-10-04T15:12:00Z"
    })
}

fn answer() -> Value {
    json!({
        "text": "One item was finished in Invoicing this week: the invoice list with filters.",
        "facts": [fact()],
        "confidence": 0.91
    })
}

const QUESTION: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y";
/// A question an approver rejected.
const REJECTED: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Z";

#[test]
fn health_and_session() {
    let health = through::<ServiceHealth>(json!({ "status": "ok", "version": "0.0.0" }));
    let session = through::<SessionRequest>(json!({ "token": "mcy_<public_id>_<secret>" }));
    assert_json_snapshot!(
        "api_health_and_session",
        json!({ "health": health, "session": session })
    );
}

#[test]
fn me() {
    let me = through::<Me>(json!({
        "user": { "id": "u_01JB2M3N4P5Q6R7S8T9V0W1X2Z", "email": "pm@example.com",
                  "display_name": "Alex", "role": "pm" },
        "projects": [{ "project": "delta", "approver": false }, { "project": "portfolio", "approver": true }],
        "token": { "public_id": "publicid0001", "expires_at": "2026-12-21T00:00:00Z" }
    }));
    assert_json_snapshot!("api_me", me);
}

#[test]
fn projects() {
    let projects = through::<ProjectList>(json!({ "projects": [
        { "project": "delta", "display_name": "Delta — invoicing", "modules": ["Invoicing", "Export", "Auth"],
          "classes": ["progress", "tested", "blocked", "health", "changes"],
          "probe": { "status": "healthy", "as_of": "2026-10-04T15:12:00Z" } },
        { "project": "omega", "display_name": "Omega — mobile app", "modules": [], "classes": ["health"],
          "probe": null }
    ]}));
    assert_json_snapshot!("api_projects", projects);
}

#[test]
fn questions() {
    let new = through::<NewQuestion>(json!({
        "text": "what's done this week?",
        "clarify": { "of": QUESTION, "option": "delta" }
    }));
    let accepted = through::<Accepted>(json!({ "id": QUESTION }));
    // asked again after an approver rejected the first; its `accepted` event names that one
    let again = through::<NewQuestion>(json!({
        "text": "what's done in billing this week?", "project": "delta", "follows": REJECTED
    }));
    let accepted_again = through::<Accepted>(json!({ "id": QUESTION, "follows": REJECTED }));
    let delivered = through::<QuestionState>(json!({
        "id": QUESTION, "status": "delivered", "project": "delta", "class": "progress", "answer": answer(),
        "refusal": null, "judge_mode": "jev", "cost_usd": 0.011, "duration_ms": 6420, "audit_id": 1834
    }));
    let running = through::<QuestionState>(json!({
        "id": QUESTION, "status": "running", "project": null, "class": null, "answer": null, "refusal": null,
        "judge_mode": null, "cost_usd": 0.0, "duration_ms": null, "audit_id": null
    }));
    let feedback = through::<Feedback>(json!({ "helpful": true, "needed_dev": false }));
    assert_json_snapshot!(
        "api_questions",
        json!({
            "new": new, "accepted": accepted, "again": again, "accepted_again": accepted_again,
            "delivered": delivered, "running": running, "feedback": feedback
        })
    );
}

#[test]
fn approvals() {
    // another person's question with its draft and flags, then the caller's own without either
    let list = through::<ApprovalList>(json!({ "approvals": [{
        "id": "ap_01JB2M3N4P5Q6R7S8T9V0W1X2Y", "question_id": QUESTION, "project": "delta",
        "class": "progress", "asker_role": "pm", "reason": "review_band", "draft": answer(),
        "flags": [{ "sentence": "One item was finished in Invoicing this week.",
                    "check": "sentence_evaluates_people", "p": 0.41 }],
        "asked_by_me": false, "status": "pending",
        "created_at": "2026-10-04T15:12:09Z", "expires_at": "2026-10-05T15:12:09Z"
    }, {
        "id": "ap_01JB2M3N4P5Q6R7S8T9V0W1X2Z", "question_id": "q_01JB2M3N4P5Q6R7S8T9V0W1X30", "project": "delta",
        "class": "tested", "asker_role": "tech_lead", "reason": "class",
        "asked_by_me": true, "status": "pending",
        "created_at": "2026-10-04T16:00:00Z", "expires_at": "2026-10-05T16:00:00Z"
    }]}));
    let reject =
        through::<ApprovalDecision>(json!({ "decision": "reject", "note": "Ask the tech lead first." }));
    let approve = through::<ApprovalDecision>(json!({ "decision": "approve" }));
    assert_json_snapshot!(
        "api_approvals",
        json!({ "list": list, "reject": reject, "approve": approve })
    );
}

#[test]
fn audit() {
    let page = through::<AuditPage>(json!({
        "items": [
            { "seq": 1834, "question_id": QUESTION, "type": "classified", "at": "2026-10-04T15:12:03Z",
              "user": "pm@example.com", "token": "publicid0001", "project": "delta",
              "payload": { "class": "progress", "p": 0.94, "checks": [] },
              "payload_sha256": "c".repeat(64), "prev_hash": "0".repeat(64), "hash": "a".repeat(64) },
            { "seq": 1835, "question_id": null, "type": "token_revoked", "at": "2026-10-04T15:13:00Z",
              "user": "admin@example.com", "token": null, "project": null, "payload": null,
              "payload_sha256": "d".repeat(64), "prev_hash": "a".repeat(64), "hash": "b".repeat(64) }
        ],
        "next": null
    }));
    assert_json_snapshot!("api_audit", page);
}

#[test]
fn admin() {
    let counts = json!({ "answered": 31, "refused": 6, "degraded": 2, "held": 3, "other": 1 });
    let none = json!({ "answered": 0, "refused": 0, "degraded": 0, "held": 0, "other": 0 });
    let overview = through::<AdminOverview>(json!({
        "window": "7d", "since": "2026-10-01T09:00:00Z", "until": "2026-10-08T09:00:00Z", "bucket": "day",
        "totals": counts,
        "series": [
            { "start": "2026-10-01T00:00:00Z", "counts": none },
            { "start": "2026-10-02T00:00:00Z", "counts": counts }
        ],
        "channels": [
            { "channel": "cli", "counts": none }, { "channel": "web", "counts": counts },
            { "channel": "mcp", "counts": none }
        ],
        "projects": [
            { "project": "delta", "counts": counts, "cost_usd": 0.42 },
            { "project": null, "counts": none, "cost_usd": 0.0 }
        ],
        "classes": [{ "class": "progress", "count": 18 }, { "class": "blocked", "count": 9 }],
        "refusals": [{ "code": "asks_for_code", "count": 4 }, { "code": "forbidden_project", "count": 2 }],
        "judge": { "jev": 38, "chat": 2, "rules": 0, "none": 3 },
        "duration_ms": { "p50": 6400, "p95": 14200, "max": 21000 },
        "cost_usd": 0.42, "askers": 4,
        "signoffs": { "pending": 1, "approved": 2, "rejected": 0, "expired": 0 },
        "spend": {
            "day": "2026-10-08", "everyone_usd": 0.12, "daily_budget_usd": 5.0, "per_user_daily_usd": 1.0,
            "people": [{ "user_id": "u_01JB2M3N4P5Q6R7S8T9V0W1X2Z", "email": "pm@example.com",
                         "display_name": "Alex", "spent_usd": 0.12 }]
        },
        "alerts": [{ "kind": "probe_silent", "project": "delta", "since": "2026-10-08T08:30:00Z",
                     "judge_mode": null, "spent_usd": null, "budget_usd": null }],
        "settings": {
            "models": [
                { "role": "judge", "backend": "openrouter", "kind": "jev", "model": "jev" },
                { "role": "composer", "backend": "openrouter", "kind": null, "model": "example/composer-1" }
            ],
            "budgets": { "per_question_usd": 0.05, "per_user_daily_usd": 1.0, "global_daily_usd": 5.0 },
            "checks": [{ "id": "asks_for_code", "stage": "input", "block_at": 0.5, "review_at": null,
                         "pass_at": null, "clarify_below": null }]
        }
    }));
    let people = through::<AdminPeople>(json!({
        "people": [{
            "id": "u_01JB2M3N4P5Q6R7S8T9V0W1X2Z", "email": "pm@example.com", "display_name": "Alex",
            "role": "pm", "created_at": "2026-09-30T10:00:00Z", "disabled_at": null,
            "grants": [
                { "project": "delta", "approver": false,
                  "classes": [{ "class": "progress", "signoff": false }, { "class": "health", "signoff": true }] },
                { "project": "gamma", "approver": false, "classes": null }
            ],
            "tokens": [{ "public_id": "publicid0001", "label": "laptop", "state": "active",
                         "created_at": "2026-09-30T10:01:00Z", "expires_at": "2026-12-29T10:01:00Z",
                         "revoked_at": null, "last_used_at": "2026-10-08T08:59:00Z" }],
            "questions_30d": 27, "last_asked_at": "2026-10-08T08:59:00Z", "spent_today_usd": 0.12
        }],
        "projects": ["delta", "gamma"],
        "classes_known": true
    }));
    let repositories = through::<AdminRepositories>(json!({
        "repositories": [
            { "project": "delta", "display_name": "Delta", "access_max": "code",
              "probe": { "status": "healthy", "access": "metadata", "access_limited_by": "token",
                         "schema": "mercury.facts/0.1", "registered_at": "2026-10-01T08:00:00Z",
                         "last_seen_at": "2026-10-08T08:59:00Z", "quarantine_reason": null,
                         "modules": ["Invoicing"], "tools": ["describe", "progress"] },
              "people": [{ "email": "pm@example.com", "display_name": "Alex", "role": "pm",
                           "approver": false, "disabled_at": null }],
              "questions_30d": 27, "last_asked_at": "2026-10-08T08:59:00Z" },
            { "project": "gamma", "display_name": "Gamma", "access_max": "metadata", "probe": null,
              "people": [], "questions_30d": 0, "last_asked_at": null }
        ]
    }));
    assert_json_snapshot!(
        "api_admin",
        json!({ "overview": overview, "people": people, "repositories": repositories })
    );
}

#[test]
fn errors() {
    let error = through::<ErrorBody>(json!({ "error": {
        "code": "rate_limited", "message": "Too many questions.", "hint": "Try again in a few minutes."
    }}));
    let bare = through::<ErrorBody>(
        json!({ "error": { "code": "internal", "message": "Something failed.", "hint": null }}),
    );
    assert_json_snapshot!("api_errors", json!([error, bare]));
}

#[test]
fn probe_registration() {
    let registration = through::<ProbeRegistration>(json!({
        "project": "delta", "schema": "mercury.facts/0.1", "access": "code", "access_limited_by": null,
        "manifest_hash": "c".repeat(64),
        "manifest": {
            "version": 0.1, "project": "delta", "display_name": "Delta — invoicing", "access": "code",
            "modules": { "Invoicing": ["src/Invoices/**"], "Export": ["src/Export/**"] },
            "module_labels": { "Export": ["export"] }, "exclude": ["**/*.pem"],
            "tests": { "frameworks": ["xunit"] }, "tracker": "github_issues", "blocked_labels": ["blocked"],
            "allow": ["progress", "tested", "blocked", "health", "changes"], "deny": [],
            "redact": [{ "pattern": "(?i)\\bexample\\s+bank\\b", "replace": "the client" }],
            "signoff": [], "judge_context": ["titles", "labels"]
        },
        "tools": ["describe", "progress", "tested", "blocked", "health", "changes"]
    }));
    assert_json_snapshot!("api_probe_registration", registration);
}

#[test]
fn probe_judge_requests_and_replies() {
    let status = through::<JudgeRequest>(json!({ "purpose": "status_from_candidates", "items": [
        { "id": "f_pull412", "candidates": ["in_review", "blocked"],
          "title": "Invoice export in the new file format", "labels": ["export", "blocked"],
          "signals": { "pr": { "state": "open", "review_requested": true, "updated_14d": true },
                       "issues": [{ "open": true, "blocked": true }], "ci": "passed" } },
        { "id": "f_issue77", "candidates": ["blocked", "not_started"], "title": null, "labels": [],
          "signals": { "pr": null, "issues": [{ "open": true, "blocked": true }], "ci": "none" } }
    ]}));
    let kind = through::<JudgeRequest>(json!({ "purpose": "kind", "items": [
        { "id": "f_pull412", "title": "Fix rounding in invoice totals", "labels": ["bug"] }
    ]}));
    let title = through::<JudgeRequest>(json!({ "purpose": "title_safety", "items": [
        { "id": "f_pull412", "title": "Call the exporter nightly" }
    ]}));
    let replies = through::<Vec<JudgeReply>>(json!([
        { "purpose": "status_from_candidates", "judge_mode": "jev", "items": [
            { "id": "f_pull412", "status": "blocked", "p": 0.81 },
            { "id": "f_issue77", "status": "blocked", "p": 0.66 }] },
        { "purpose": "kind", "judge_mode": "jev", "items": [{ "id": "f_pull412", "kind": "fix", "p": 0.93 }] },
        { "purpose": "title_safety", "judge_mode": "chat", "items": [{ "id": "f_pull412", "p": 0.12 }] }
    ]));
    assert_json_snapshot!(
        "probe_judge",
        json!({ "requests": [status, kind, title], "replies": replies })
    );
}

#[test]
fn an_answered_stream() {
    let events = through::<Vec<Event>>(json!([
        { "event": "accepted", "data": { "id": QUESTION } },
        { "event": "project", "data": { "project": "delta", "p": 0.99, "source": "explicit" } },
        { "event": "classified", "data": { "class": "progress", "p": 0.94, "checks": [
            { "id": "asks_for_code", "source": "jev", "p": 0.02, "verdict": "pass" }] } },
        { "event": "plan", "data": { "calls": [
            { "tool": "progress", "args": { "since": "2026-09-28", "limit": 20 } }] } },
        { "event": "tool_checked", "data": { "call": 0, "checks": [
            { "id": "args_match_question", "source": "chat", "p": 0.95, "verdict": "pass" },
            { "id": "scope", "source": "rule", "verdict": "pass" }] } },
        { "event": "tool_result", "data": { "call": 0, "items": 2, "withheld_fields": ["/items/1/title"],
                                            "access": "code" } },
        { "event": "composing", "data": {} },
        { "event": "judged", "data": { "sentences": 2, "stripped": 1, "held": 0 } },
        { "event": "answer", "data": answer() },
        { "event": "done", "data": { "duration_ms": 1900, "cost_usd": 0.004, "judge_mode": "jev",
                                     "composer": "model", "audit_id": 1834 } }
    ]));
    assert_json_snapshot!("events_answered", events);
}

#[test]
fn an_open_question_read_from_the_code() {
    let events = through::<Vec<Event>>(json!([
        { "event": "accepted", "data": { "id": QUESTION } },
        { "event": "project", "data": { "project": "delta", "p": 0.99, "source": "explicit" } },
        { "event": "classified", "data": { "class": "open", "p": 0.88, "checks": [
            { "id": "asks_for_code", "source": "jev", "p": 0.04, "verdict": "pass" }] } },
        { "event": "plan", "data": { "calls": [
            { "tool": "progress", "args": { "module": "Export" } },
            { "tool": "ask_code", "args": { "question": "How does the export pick the file format?" } }] } },
        // nothing came back to check, so no judge gave a probability
        { "event": "tool_result", "data": { "call": 0, "items": 0, "withheld_fields": [],
                                            "access": "code_model" } },
        { "event": "tool_result", "data": { "call": 1, "items": 2, "withheld_fields": [],
                                            "access": "code_model", "p_min": 0.93 } },
        { "event": "composing", "data": {} },
        { "event": "judged", "data": { "sentences": 2, "stripped": 0, "held": 0, "p_min": 0.87 } },
        { "event": "answer", "data": {
            "text": "The export writes one file per invoice batch. Totals are rounded once, after the discounts.",
            "facts": [], "confidence": 0.82 } },
        { "event": "done", "data": { "duration_ms": 9800, "cost_usd": 0.0074, "judge_mode": "jev",
                                     "composer": "model", "audit_id": 1852 } }
    ]));
    assert_json_snapshot!("events_open", events);
}

#[test]
fn streams_that_end_early() {
    let events = through::<Vec<Event>>(json!([
        { "event": "refused", "data": { "code": "asks_for_code", "message": "I can't show code.",
            "hint": "I can tell you whether the export is done and tested.", "cost_usd": 0.001,
            "judge_mode": "jev" } },
        { "event": "refused", "data": { "code": "confidential", "message": "I can't share confidential information.",
            "hint": "I can tell you how far the export is and what blocks it.", "cost_usd": 0.0009,
            "judge_mode": "jev" } },
        { "event": "refused", "data": { "code": "forbidden_project", "message": "You can't ask about that project.",
            "hint": "You can ask about delta.", "cost_usd": 0.0, "judge_mode": null } },
        { "event": "clarify", "data": { "kind": "project", "question": "Which project do you mean?",
            "options": [{ "id": "delta", "label": "Delta — invoicing" }, { "id": "omega", "label": "Omega" }],
            "cost_usd": 0.0008, "judge_mode": "chat" } },
        { "event": "pending_signoff", "data": { "approval_id": "ap_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
            "reason": "class" } },
        { "event": "error", "data": { "code": "probe_unavailable", "message": "The project's probe didn't answer.",
            "cost_usd": 0.0021, "judge_mode": "rules" } },
        { "event": "done", "data": { "duration_ms": 2400, "cost_usd": 0.0012, "judge_mode": "rules",
                                     "composer": "template", "audit_id": 1840 } }
    ]));
    assert_json_snapshot!("events_ended_early", events);
}
