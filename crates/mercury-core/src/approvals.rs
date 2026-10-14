//! Sign-off: an answer the policy or a review band holds waits for an approver of its project,
//! who releases it unchanged or rejects it with a note; nobody decides on their own question, and
//! an answer nobody decides on within a day is refused.
//!
//! A held answer is the `answer` it would have been, kept in `approvals` with the `pending_signoff`
//! that says so, in one transaction. A decision is an audit row of its own, then the question's
//! stream goes on (`answer` and `done`, or `refused`), all in one transaction, so a stream that
//! waited after `pending_signoff` reads the rest, and the first decision wins.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::api::{
    Answer, Approval, ApprovalDecision, ApprovalList, ApprovalStatus, Decision, ErrorCode, Flag, Refusal,
    RefusalCode, SignoffReason, Usd,
};
use mercury_schema::config::PORTFOLIO;
use mercury_schema::events::{ComposerMode, Done, Event, PendingSignoff, Refused};
use mercury_schema::ids::ApprovalId;
use mercury_schema::{JudgeMode, ProjectId, QuestionClass, QuestionId, Role};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use crate::audit::{self, AuditError, Recorded};
use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::Caller;
use crate::limits;
use crate::questions::{Emitter, Hub, Questions};
use crate::session;

/// How long a held answer waits for a decision.
pub const TTL: TimeDelta = TimeDelta::hours(24);
/// The longest note, in characters, once trimmed.
pub const MAX_NOTE_CHARS: usize = 500;

/// What a rejection tells the asker besides the approver's note.
pub const REJECTED: &str = "An approver didn't release this answer.";
/// What an answer nobody decided on tells the asker.
pub const EXPIRED: &str = "No approver released this answer within 24 hours.";
pub const EXPIRED_HINT: &str = "Ask again when an approver is available.";

/// An answer the orchestrator holds for an approver.
#[derive(Debug, Clone)]
pub struct Held {
    pub reason: SignoffReason,
    /// The `answer` approving delivers, byte for byte.
    pub draft: Answer,
    pub flags: Vec<Flag>,
    /// Who wrote the draft, for the `done` that follows an approval.
    pub composer: ComposerMode,
}

/// `approvals.reason_json`: why the answer waits and what the approver is shown besides the draft.
#[derive(Debug, Serialize, Deserialize)]
struct Reason {
    reason: SignoffReason,
    flags: Vec<Flag>,
    composer: ComposerMode,
}

#[derive(Debug, thiserror::Error)]
pub enum HoldError {
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("an approval id can't be made: {0}")]
    Id(String),
}

impl From<sqlx::Error> for HoldError {
    fn from(err: sqlx::Error) -> Self {
        Self::Audit(AuditError::Write(err))
    }
}

/// Holds the answer: its `approvals` row and the question's `pending_signoff`, one transaction.
pub async fn hold(
    db: &Db,
    events: &mut Emitter,
    held: Held,
    now: DateTime<Utc>,
) -> Result<Recorded<Event>, HoldError> {
    let id = new_approval_id(now).map_err(HoldError::Id)?;
    let reason = Reason {
        reason: held.reason,
        flags: held.flags,
        composer: held.composer,
    };
    let reason_json = serde_json::to_string(&reason).map_err(|_| AuditError::Payload)?;
    let draft = serde_json::to_string(&held.draft).map_err(|_| AuditError::Payload)?;
    let (aid, qid) = (id.as_str(), events.question().as_str().to_owned());
    let (created, expires) = (db::time(now), db::time(now + TTL));
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!(
        "INSERT INTO approvals (id, question_id, reason_json, draft_answer, status, created_at, expires_at)
         VALUES (?, ?, ?, ?, 'pending', ?, ?)",
        aid,
        qid,
        reason_json,
        draft,
        created,
        expires
    )
    .execute(&mut *tx)
    .await?;
    let event = Event::PendingSignoff(PendingSignoff {
        approval_id: id,
        reason: held.reason,
    });
    let pending = events.record_in(&mut tx, event, now).await?;
    let recorded = pending.commit(tx).await?;
    events.written(recorded.seq());
    Ok(recorded)
}

/// Whether someone besides the asker, not disabled, approves `project`: without one, a review
/// band can't be held, since nobody could release it. Only a project manager's or a tech lead's
/// flag counts (`identity::may_approve`): one left on another role releases nothing, so it doesn't
/// hold a sentence either.
pub async fn has_approver(db: &Db, project: &ProjectId, asker: &str) -> Result<bool, sqlx::Error> {
    let project = project.as_str();
    let found: Option<i64> = sqlx::query_scalar!(
        "SELECT 1 FROM user_projects up JOIN users u ON u.id = up.user_id
         WHERE up.project = ? AND up.approver = 1 AND u.role IN ('pm', 'tech_lead')
           AND up.user_id <> ? AND u.disabled_at IS NULL LIMIT 1",
        project,
        asker
    )
    .fetch_optional(db.pool())
    .await?;
    Ok(found.is_some())
}

fn new_approval_id(now: DateTime<Utc>) -> Result<ApprovalId, String> {
    let mut random = [0; 10];
    getrandom::fill(&mut random).map_err(|err| err.to_string())?;
    let millis = u64::try_from(now.timestamp_millis()).unwrap_or(0);
    ApprovalId::parse(&format!("ap_{}", crate::admin::encode_ulid(millis, random)))
        .map_err(|err| err.to_string())
}

fn parse_name<T: DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

// ---------------------------------------------------------------------------------------------
// Listing

#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("a stored approval doesn't have the expected shape: {0}")]
    Corrupt(&'static str),
}

/// A row of the pending list as the database has it.
struct PendingRow {
    id: String,
    question_id: String,
    reason_json: String,
    draft_answer: String,
    created_at: String,
    expires_at: String,
    project: Option<String>,
    class: Option<String>,
    asker: String,
    role: String,
}

/// The pending, unexpired answers of every project `user` approves (`portfolio` for a question
/// without a project), oldest first, their own questions included without draft and flags. The
/// flag counts only while `user` is a project manager or a tech lead (`identity::may_approve`).
pub async fn pending(db: &Db, user: &str, now: DateTime<Utc>) -> Result<Vec<Approval>, ListError> {
    let at = db::time(now);
    let rows = sqlx::query_as!(
        PendingRow,
        r#"SELECT a.id, a.question_id, a.reason_json, a.draft_answer, a.created_at, a.expires_at,
                  q.project, q.class, q.user_id AS asker, u.role
           FROM approvals a
           JOIN questions q ON q.id = a.question_id
           JOIN users u ON u.id = q.user_id
           WHERE a.status = 'pending' AND a.expires_at > ?
             AND COALESCE(q.project, 'portfolio') IN
                 (SELECT up.project FROM user_projects up JOIN users me ON me.id = up.user_id
                  WHERE up.user_id = ? AND up.approver = 1 AND me.role IN ('pm', 'tech_lead'))
           ORDER BY a.created_at, a.id"#,
        at,
        user
    )
    .fetch_all(db.pool())
    .await?;
    let mut approvals = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.id.clone();
        match listed(row, user) {
            Ok(approval) => approvals.push(approval),
            // one row that can't be read leaves the others listed; its id says which
            Err(err) => {
                tracing::warn!(approval = %id, error = %err, "a held answer can't be read; it is left out of the list");
            }
        }
    }
    Ok(approvals)
}

/// One row of the pending list, read whole or not at all. The caller's own question comes without
/// its draft and its flags: the asker reads nothing of a held answer before the decision, whatever
/// the client.
fn listed(row: PendingRow, user: &str) -> Result<Approval, ListError> {
    let corrupt = ListError::Corrupt;
    let reason: Reason = serde_json::from_str(&row.reason_json).map_err(|_| corrupt("its reason"))?;
    let draft: Answer = serde_json::from_str(&row.draft_answer).map_err(|_| corrupt("its draft"))?;
    let asked_by_me = row.asker == user;
    let (draft, flags) = if asked_by_me {
        (None, None)
    } else {
        (Some(draft), Some(reason.flags))
    };
    Ok(Approval {
        id: ApprovalId::parse(&row.id).map_err(|_| corrupt("its id"))?,
        question_id: QuestionId::parse(&row.question_id).map_err(|_| corrupt("its question"))?,
        project: row
            .project
            .as_deref()
            .filter(|p| *p != PORTFOLIO)
            .map(ProjectId::parse)
            .transpose()
            .map_err(|_| corrupt("its project"))?,
        class: row
            .class
            .as_deref()
            .and_then(parse_name::<QuestionClass>)
            .ok_or(corrupt("its class"))?,
        asker_role: parse_name::<Role>(&row.role).ok_or(corrupt("its asker's role"))?,
        reason: reason.reason,
        draft,
        flags,
        asked_by_me,
        status: ApprovalStatus::Pending,
        created_at: db::parse_time(&row.created_at).ok_or(corrupt("its time"))?,
        expires_at: db::parse_time(&row.expires_at).ok_or(corrupt("its expiry"))?,
    })
}

// ---------------------------------------------------------------------------------------------
// Deciding

#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    /// No such approval, or one of a project the caller doesn't approve: the same either way.
    #[error("there is no such approval")]
    NotFound,
    #[error("the caller asked this question")]
    OwnQuestion,
    #[error("the approval is already {0:?}")]
    Decided(ApprovalStatus),
    #[error("the approval expired")]
    Expired,
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("a stored approval doesn't have the expected shape: {0}")]
    Corrupt(&'static str),
}

impl From<sqlx::Error> for DecideError {
    fn from(err: sqlx::Error) -> Self {
        Self::Audit(AuditError::Write(err))
    }
}

/// An approval and its question, as a decision reads them.
struct Loaded {
    id: String,
    question: QuestionId,
    project: Option<ProjectId>,
    asker: String,
    status: String,
    expires_at: String,
    /// The stored reason and draft, read only by an approval: an expiry or a rejection needs
    /// neither, so a row whose texts are broken still ends.
    reason: String,
    draft: String,
    asked_at: String,
    cost: Usd,
    judge_mode: Option<JudgeMode>,
}

async fn load(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<Option<Loaded>, DecideError> {
    let row = sqlx::query!(
        r#"SELECT a.id, a.question_id, a.reason_json, a.draft_answer, a.status, a.expires_at,
                  q.user_id AS asker, q.project, q.cost_usd, q.judge_mode, q.created_at AS asked_at
           FROM approvals a JOIN questions q ON q.id = a.question_id WHERE a.id = ?"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let corrupt = DecideError::Corrupt;
    Ok(Some(Loaded {
        id: row.id,
        question: QuestionId::parse(&row.question_id).map_err(|_| corrupt("its question"))?,
        project: row
            .project
            .as_deref()
            .map(ProjectId::parse)
            .transpose()
            .map_err(|_| corrupt("its project"))?,
        asker: row.asker,
        status: row.status,
        expires_at: row.expires_at,
        reason: row.reason_json,
        draft: row.draft_answer,
        asked_at: row.asked_at,
        cost: Usd::new(row.cost_usd).ok_or(corrupt("its question's cost"))?,
        judge_mode: row.judge_mode.as_deref().and_then(parse_name::<JudgeMode>),
    }))
}

/// Whether `user` approves the project of a question (`portfolio` for one without a project): the
/// flag, while they are a project manager or a tech lead (`identity::may_approve`). A flag left on
/// another role is refused as no flag is, so the caller can't tell it from an unknown approval.
async fn approves(
    tx: &mut Transaction<'_, Sqlite>,
    user: &str,
    project: Option<&ProjectId>,
) -> Result<bool, sqlx::Error> {
    let project = project.map_or(PORTFOLIO, ProjectId::as_str);
    let flag: Option<i64> = sqlx::query_scalar!(
        "SELECT up.approver FROM user_projects up JOIN users u ON u.id = up.user_id
         WHERE up.user_id = ? AND up.project = ? AND u.role IN ('pm', 'tech_lead')",
        user,
        project
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(flag == Some(1))
}

/// A decision's or an expiry's row: the project, and in the payload what happened to which
/// question. No question id of its own: it isn't one of the question's events.
async fn decision_row(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    loaded: &Loaded,
    payload: serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), AuditError> {
    let mut payload = payload;
    payload["approval_id"] = loaded.id.clone().into();
    payload["question_id"] = loaded.question.as_str().into();
    audit::action_in(tx, kind, loaded.project.as_ref(), &payload, now).await?;
    Ok(())
}

/// The approver `caller` decides on the approval `id`: approve delivers the draft unchanged,
/// reject refuses the question with the note as its hint. `note` is already checked.
pub async fn decide(
    db: &Db,
    hub: &Hub,
    caller: &Caller,
    id: &str,
    decision: Decision,
    note: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), DecideError> {
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let Some(loaded) = load(&mut tx, id).await? else {
        return Err(DecideError::NotFound);
    };
    if !approves(&mut tx, &caller.user_id, loaded.project.as_ref()).await? {
        return Err(DecideError::NotFound);
    }
    if loaded.asker == caller.user_id {
        return Err(DecideError::OwnQuestion);
    }
    if loaded.status != "pending" {
        let status =
            parse_name::<ApprovalStatus>(&loaded.status).ok_or(DecideError::Corrupt("its status"))?;
        return Err(DecideError::Decided(status));
    }
    let mut events = Emitter::resumed(
        db.clone(),
        hub.clone(),
        loaded.question.clone(),
        loaded.project.clone(),
    );
    if expired(&loaded, now) {
        let seq = expire_in(&mut tx, &mut events, &loaded, now).await?;
        tx.commit().await?;
        events.written(seq);
        return Err(DecideError::Expired);
    }
    let decided = db::time(now);
    let status = match decision {
        Decision::Approve => "approved",
        Decision::Reject => "rejected",
    };
    sqlx::query!(
        "UPDATE approvals SET status = ?, decided_by = ?, note = ?, decided_at = ? WHERE id = ?",
        status,
        caller.user_id,
        note,
        decided,
        loaded.id
    )
    .execute(&mut *tx)
    .await?;
    // who decided, as the audit's `user` and `token` filters read a row without a question
    let payload = serde_json::json!({
        "decision": status,
        "decided_by": caller.user_id,
        "email": caller.email,
        "token": caller.token.public_id,
        "note": note,
    });
    decision_row(&mut tx, "approval_decided", &loaded, payload, now).await?;
    let last = match decision {
        Decision::Approve => {
            let draft: Answer =
                serde_json::from_str(&loaded.draft).map_err(|_| DecideError::Corrupt("its draft"))?;
            let reason: Reason =
                serde_json::from_str(&loaded.reason).map_err(|_| DecideError::Corrupt("its reason"))?;
            let qid = loaded.question.as_str();
            sqlx::query!(
                "UPDATE questions SET answer_redacted = ? WHERE id = ?",
                draft.text,
                qid
            )
            .execute(&mut *tx)
            .await?;
            let answer = events.record_in(&mut tx, Event::Answer(draft), now).await?;
            let asked =
                db::parse_time(&loaded.asked_at).ok_or(DecideError::Corrupt("its question's time"))?;
            let done = Done {
                duration_ms: u64::try_from((now - asked).num_milliseconds()).unwrap_or(0),
                cost_usd: loaded.cost,
                judge_mode: loaded.judge_mode.unwrap_or(JudgeMode::Rules),
                composer: reason.composer,
                audit_id: answer.seq(),
            };
            events.record_in(&mut tx, Event::Done(done), now).await?
        }
        Decision::Reject => {
            let refusal = Refusal {
                code: RefusalCode::RejectedByApprover,
                message: REJECTED.to_owned(),
                hint: note.unwrap_or_default().to_owned(),
            };
            let refused = Refused::new(refusal, loaded.cost, loaded.judge_mode);
            events.record_in(&mut tx, Event::Refused(refused), now).await?
        }
    };
    let recorded = last.commit(tx).await?;
    events.written(recorded.seq());
    Ok(())
}

fn expired(loaded: &Loaded, now: DateTime<Utc>) -> bool {
    db::parse_time(&loaded.expires_at).is_none_or(|at| at <= now)
}

/// An approval nobody decided on in time: `approval_expired`, then the question refused. Gives
/// the `seq` of the refusal, for the caller to wake the streams once it has committed.
async fn expire_in(
    tx: &mut Transaction<'_, Sqlite>,
    events: &mut Emitter,
    loaded: &Loaded,
    now: DateTime<Utc>,
) -> Result<u64, DecideError> {
    let decided = db::time(now);
    sqlx::query!(
        "UPDATE approvals SET status = 'expired', decided_at = ? WHERE id = ?",
        decided,
        loaded.id
    )
    .execute(&mut **tx)
    .await?;
    decision_row(tx, "approval_expired", loaded, serde_json::json!({}), now).await?;
    let refusal = Refusal {
        code: RefusalCode::RejectedByApprover,
        message: EXPIRED.to_owned(),
        hint: EXPIRED_HINT.to_owned(),
    };
    let refused = Refused::new(refusal, loaded.cost, loaded.judge_mode);
    let pending = events.record_in(tx, Event::Refused(refused), now).await?;
    Ok(pending.seq())
}

/// Refuses every held answer whose day is over; gives how many. Each one in its own transaction,
/// so one that fails doesn't keep the others waiting.
pub async fn expire_due(db: &Db, hub: &Hub, now: DateTime<Utc>) -> Result<usize, DecideError> {
    let at = db::time(now);
    let due: Vec<String> = sqlx::query_scalar!(
        "SELECT id FROM approvals WHERE status = 'pending' AND expires_at <= ? ORDER BY expires_at",
        at
    )
    .fetch_all(db.pool())
    .await?;
    let mut expired = 0;
    for id in due {
        match expire_one(db, hub, &id, now).await {
            Ok(true) => expired += 1,
            Ok(false) => {}
            // the next item's asker isn't kept waiting for this one; the next sweep tries it again
            Err(err) => {
                tracing::warn!(approval = %id, error = %err, "a held answer that expired can't be refused");
            }
        }
    }
    Ok(expired)
}

/// One due item refused in its own transaction; `false` when it was decided meanwhile.
async fn expire_one(db: &Db, hub: &Hub, id: &str, now: DateTime<Utc>) -> Result<bool, DecideError> {
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let Some(loaded) = load(&mut tx, id).await?.filter(|l| l.status == "pending") else {
        return Ok(false);
    };
    let mut events = Emitter::resumed(
        db.clone(),
        hub.clone(),
        loaded.question.clone(),
        loaded.project.clone(),
    );
    let seq = expire_in(&mut tx, &mut events, &loaded, now).await?;
    tx.commit().await?;
    events.written(seq);
    Ok(true)
}

// ---------------------------------------------------------------------------------------------
// The routes

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    status: Option<String>,
}

/// `GET /v1/approvals` and `POST /v1/approvals/{id}`, by Bearer token or by the web client's
/// session; a decision by cookie passes the session's check on where it comes from.
pub fn routes(state: Questions) -> Router {
    let sessions = state.sessions.clone();
    let decide = Router::new()
        .route("/v1/approvals/{id}", post(decided))
        .layer(middleware::from_fn(limits::body_cap))
        .with_state(state.clone());
    Router::new()
        .route("/v1/approvals", get(list))
        .with_state(state)
        .merge(decide)
        .layer(middleware::from_fn_with_state(sessions, session::resolve))
}

fn error(status: StatusCode, message: &str, hint: &str) -> ApiError {
    ApiError {
        status,
        code: ErrorCode::InvalidRequest,
        message: message.to_owned(),
        hint: hint.to_owned(),
        retry_after: None,
    }
}

fn internal(err: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %err, "an approvals route failed");
    ApiError::internal()
}

async fn list(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<Params>, QueryRejection>,
) -> Result<Json<ApprovalList>, ApiError> {
    let params = query.map(|Query(p)| p).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "The query has a parameter the approvals don't take, or one given twice.",
            "Ask for status=pending.",
        )
    })?;
    if params.status.as_deref().is_some_and(|s| s != "pending") {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "The parameter `status` can only be `pending`.",
            "Ask for status=pending.",
        ));
    }
    let approvals = pending(&state.db, &caller.user_id, db::now())
        .await
        .map_err(internal)?;
    Ok(Json(ApprovalList { approvals }))
}

/// A note as a rejection takes it: trimmed, 1 to 500 characters, no control characters.
fn note(note: Option<&str>, decision: Decision) -> Result<Option<String>, ApiError> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    let Some(note) = note else {
        return match decision {
            Decision::Approve => Ok(None),
            Decision::Reject => Err(error(
                StatusCode::BAD_REQUEST,
                "A rejection needs a note.",
                "Say what the asker should do instead; they see the note.",
            )),
        };
    };
    if note.chars().count() > MAX_NOTE_CHARS || note.chars().any(char::is_control) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "The note must be 1 to 500 characters on one line.",
            "Write a shorter note.",
        ));
    }
    Ok(Some(note.to_owned()))
}

async fn decided(
    State(state): State<Questions>,
    caller: Caller,
    Path(id): Path<String>,
    parsed: Result<Json<ApprovalDecision>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = parsed.map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "The request body isn't the JSON this route takes.",
            "See /v1/openapi.json for the body.",
        )
    })?;
    let note = note(body.note.as_deref(), body.decision)?;
    let not_found = || {
        error(
            StatusCode::NOT_FOUND,
            "There is no such approval.",
            "Use an id from the pending list.",
        )
    };
    if ApprovalId::parse(&id).is_err() {
        return Err(not_found());
    }
    let result = decide(
        &state.db,
        &state.hub,
        &caller,
        &id,
        body.decision,
        note.as_deref(),
        db::now(),
    )
    .await;
    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(DecideError::NotFound) => Err(not_found()),
        Err(DecideError::OwnQuestion) => Err(error(
            StatusCode::FORBIDDEN,
            "You can't decide on your own question.",
            "Another approver of the project decides on it.",
        )),
        Err(DecideError::Decided(_)) => Err(error(
            StatusCode::CONFLICT,
            "The answer has already been decided on.",
            "Refresh the pending list.",
        )),
        Err(DecideError::Expired) => Err(error(
            StatusCode::CONFLICT,
            "The answer waited longer than 24 hours, so it was refused.",
            "Refresh the pending list.",
        )),
        Err(err) => Err(internal(err)),
    }
}
