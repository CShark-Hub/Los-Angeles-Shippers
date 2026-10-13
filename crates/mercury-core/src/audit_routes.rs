//! `GET /v1/audit` and `GET /v1/audit/export`: the audit trail, an admin's alone. Anyone else is
//! refused before a row is read, whichever client asks; a person sees what happened to their own
//! question where they asked it. The export is streamed, and is itself an audit row, written
//! before anything is sent.

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use futures_util::TryStreamExt;
use mercury_schema::api::{AuditPage, ErrorCode, QuestionStatus};
use mercury_schema::ids::{Email, TokenPublicId};
use mercury_schema::{ProjectId, QuestionClass, QuestionId, Role};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::audit::{self, Filter};
use crate::error::ApiError;
use crate::identity::Caller;
use crate::questions::Questions;
use crate::{db, session};

/// Rows per page when `limit` is left out, and the most a page holds.
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;

/// The query of both routes; each refuses the other's own parameters.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    project: Option<String>,
    user: Option<String>,
    token: Option<String>,
    question: Option<String>,
    class: Option<String>,
    outcome: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
    format: Option<String>,
}

pub fn routes(state: Questions) -> Router {
    let sessions = state.sessions.clone();
    Router::new()
        .route("/v1/audit", get(page))
        .route("/v1/audit/export", get(export))
        .with_state(state)
        // the cookie becomes the caller, as for the questions' routes
        .layer(middleware::from_fn_with_state(sessions, session::resolve))
}

fn invalid(message: String, hint: &str) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: ErrorCode::InvalidRequest,
        message,
        hint: hint.to_owned(),
        retry_after: None,
    }
}

/// A parameter that doesn't parse; the message names it, never echoes its value.
fn bad(name: &str, shape: &str) -> ApiError {
    invalid(
        format!("The parameter `{name}` isn't {shape}."),
        "See /v1/openapi.json for the audit's parameters.",
    )
}

fn internal(err: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %err, "an audit route failed");
    ApiError::internal()
}

fn params(query: Result<Query<Params>, QueryRejection>) -> Result<Params, ApiError> {
    query.map(|Query(params)| params).map_err(|_| {
        invalid(
            "The query has a parameter the audit doesn't take, or one given twice.".to_owned(),
            "See /v1/openapi.json for the audit's parameters.",
        )
    })
}

fn parsed<T>(
    value: Option<&str>,
    name: &str,
    shape: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Option<T>, ApiError> {
    value
        .map(|raw| parse(raw.trim()).ok_or_else(|| bad(name, shape)))
        .transpose()
}

/// A wire name of an enum, `delivered` say.
fn named<T: DeserializeOwned>(raw: &str) -> Option<T> {
    serde_json::from_value(Value::String(raw.to_owned())).ok()
}

fn filter(params: &Params) -> Result<Filter, ApiError> {
    // RFC 3339, or a date for its midnight UTC: the admin commands read a time the same way
    let moment = |raw: &str| crate::cli::moment(raw).ok();
    Ok(Filter {
        project: parsed(params.project.as_deref(), "project", "a project id", |p| {
            ProjectId::parse(p).ok()
        })?,
        user: parsed(params.user.as_deref(), "user", "an e-mail address", |u| {
            Email::parse(u).ok()
        })?,
        token: parsed(params.token.as_deref(), "token", "a token's public id", |t| {
            TokenPublicId::parse(t).ok()
        })?,
        question: parsed(params.question.as_deref(), "question", "a question id", |q| {
            QuestionId::parse(q).ok()
        })?,
        class: parsed(
            params.class.as_deref(),
            "class",
            "a question class",
            named::<QuestionClass>,
        )?,
        outcome: parsed(
            params.outcome.as_deref(),
            "outcome",
            "a question status",
            named::<QuestionStatus>,
        )?,
        from: parsed(
            params.from.as_deref(),
            "from",
            "an RFC 3339 time or a date",
            moment,
        )?,
        to: parsed(params.to.as_deref(), "to", "an RFC 3339 time or a date", moment)?,
    })
}

/// The audit is an admin's: anyone else is refused before a row is read. The admin panel's routes
/// refuse the same way.
pub(crate) fn admin_only(caller: &Caller, message: &str, hint: &str) -> Result<(), ApiError> {
    if caller.role == Role::Admin {
        return Ok(());
    }
    Err(ApiError {
        status: StatusCode::FORBIDDEN,
        code: ErrorCode::InvalidRequest,
        message: message.to_owned(),
        hint: hint.to_owned(),
        retry_after: None,
    })
}

async fn page(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<Params>, QueryRejection>,
) -> Result<Json<AuditPage>, ApiError> {
    admin_only(
        &caller,
        "Only an admin reads the audit.",
        "What happened to your own question is shown where you asked it; ask an admin for anything else.",
    )?;
    let params = params(query)?;
    if params.format.is_some() {
        return Err(bad("format", "taken here; it belongs to /v1/audit/export"));
    }
    let filter = filter(&params)?;
    let limit = parsed(params.limit.as_deref(), "limit", "a number from 1 to 500", |l| {
        l.parse::<u32>().ok().filter(|l| (1..=MAX_LIMIT).contains(l))
    })?
    .unwrap_or(DEFAULT_LIMIT);
    let after = parsed(
        params.cursor.as_deref(),
        "cursor",
        "a cursor this route gave",
        |c| c.parse::<u64>().ok(),
    )?
    .unwrap_or(0);
    // one row more than the page, to know whether another page follows
    let mut items = audit::rows(&state.db, &filter, after, None, limit + 1)
        .await
        .map_err(internal)?;
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next = more
        .then(|| items.last().map(|item| item.seq.to_string()))
        .flatten();
    Ok(Json(AuditPage { items, next }))
}

async fn export(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<Params>, QueryRejection>,
) -> Result<Response, ApiError> {
    admin_only(
        &caller,
        "Only an admin may export the audit.",
        "Ask an admin for an export.",
    )?;
    let params = params(query)?;
    if params.limit.is_some() || params.cursor.is_some() {
        return Err(invalid(
            "The export takes no `limit` or `cursor`: it streams every matching row.".to_owned(),
            "Leave them out, and narrow the export with the filters instead.",
        ));
    }
    if params.format.as_deref().map(str::trim) != Some("jsonl") {
        return Err(bad("format", "`jsonl`"));
    }
    let filter = filter(&params)?;
    let db = state.db.clone();
    // the rows up to the last one now; the export's own row comes after them
    let upto = audit::last_seq(&db).await.map_err(internal)?;
    let payload = json!({
        "by": "api",
        "token": caller.token.public_id,
        "filters": filter.recorded(),
        "upto": upto,
    });
    // no project on the row: an export is an admin's act, not something that happened in a
    // project; the filters it used are in the payload
    audit::action(&db, "audit_exported", None, &payload, db::now())
        .await
        .map_err(internal)?;
    let lines = futures_util::stream::try_unfold(Some(0_u64), move |after| {
        let db = db.clone();
        let filter = filter.clone();
        async move {
            let Some(after) = after else {
                return Ok::<_, audit::ReadError>(None);
            };
            // the lines `admin audit export` writes: each row's time and payload as stored, so the
            // file re-digests byte for byte
            let batch = audit::export_batch(&db, &filter, after, upto).await?;
            if batch.rows == 0 {
                return Ok(None);
            }
            Ok(Some((Bytes::from(batch.lines), batch.next)))
        }
    });
    // a row that breaks off the stream ends the download early; the log says why
    let lines = lines.inspect_err(|err| tracing::error!(error = %err, "the audit export broke off"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/jsonl"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"mercury-audit.jsonl\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(lines),
    )
        .into_response())
}
