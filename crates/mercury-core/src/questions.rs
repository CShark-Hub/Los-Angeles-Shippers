//! The questions API: ask, follow a question's events, read where it stands, give feedback, and
//! who-am-I and my projects.
//!
//! Every event of a question is an audit row written before anyone can read it ([`Emitter`]), and
//! the stream reads the rows, so a replay after a reconnect is the same events in the same order,
//! once. The text a person typed is kept redacted, and never logged.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRef, Path, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use chrono::{DateTime, TimeDelta, Utc};
use futures_util::stream;
use mercury_schema::api::{
    Accepted, Answer, ClarifyAnswer, ErrorCode, Feedback, Grant as ApiGrant, Me, MeToken, MeUser,
    NewQuestion, ProbeState, ProbeStatus as ApiProbeStatus, ProjectList, ProjectSummary, QuestionState,
    QuestionStatus, Refusal, RefusalCode, Usd,
};
use mercury_schema::config::PORTFOLIO;
use mercury_schema::events::{Event, EventError};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{
    Email, JudgeMode, ProjectId, QuestionClass, QuestionId, Redactor, TokenPublicId, UserId,
};
use serde::de::DeserializeOwned;
use sqlx::{Sqlite, Transaction};
use tokio::sync::{mpsc, watch};

use crate::audit::{self, AuditError, Recorded};
use crate::config::{self, Config};
use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::{Caller, Channel};
use crate::limits::{self, PerToken, Rate, RateLimiter};
use crate::orchestrator::budget::{self, Cap};
use crate::policy::EffectivePolicy;
use crate::probes::ProbeStatus;
use crate::session::{self, SessionConfig, Sessions};

/// The longest question, in characters, once trimmed.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// The longest `Idempotency-Key`.
pub const MAX_KEY_LEN: usize = 128;
/// How long an `Idempotency-Key` is remembered.
pub const KEY_TTL: TimeDelta = TimeDelta::hours(24);
/// How often a quiet stream sends a comment, so proxies keep it open.
pub const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// How often a waiting stream looks at its question again, so a question that ended without a row
/// to wake on (its last event couldn't be written) still ends its streams.
pub const RECHECK: Duration = Duration::from_secs(5);

/// What a `follows` that doesn't name the caller's own question rejected by an approver gets: one
/// message and hint for every reason, so nothing is told of a question that isn't theirs.
pub const FOLLOWS_REFUSED: (&str, &str) = (
    "The question to ask again isn't one of yours that an approver rejected.",
    "Name your own question that ended with an approver's rejection, or ask without it.",
);

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const LAST_EVENT_ID: &str = "last-event-id";

/// The events after which a stream ends.
const TERMINAL: [&str; 5] = ["refused", "clarify", "pending_signoff", "done", "error"];

// ---------------------------------------------------------------------------------------------
// Who writes a question's events

/// Wakes every stream when an event of any question is written; each stream then reads its own.
#[derive(Debug, Clone)]
pub struct Hub(Arc<watch::Sender<u64>>);

impl Default for Hub {
    fn default() -> Self {
        Self(Arc::new(watch::channel(0).0))
    }
}

impl Hub {
    fn written(&self, seq: u64) {
        self.0.send_replace(seq);
    }

    /// Wakes every stream without a new row: a question's row changed, its events didn't.
    fn wake(&self) {
        self.0.send_modify(|_| {});
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
}

/// Writes one question's events: each one an audit row and the question's row brought up to date
/// in one transaction, then the streams woken. A reader who has seen an event therefore reads the
/// state it left, and "no audit, no answer": an event whose row fails is never sent.
#[derive(Debug, Clone)]
pub struct Emitter {
    db: Db,
    hub: Hub,
    question: QuestionId,
    project: Option<ProjectId>,
}

impl Emitter {
    /// The writer of one question's events, waking the streams of `hub`.
    pub fn new(db: Db, hub: Hub, question: QuestionId) -> Self {
        Self {
            db,
            hub,
            question,
            project: None,
        }
    }

    /// The writer of a question that already has its project: one a decision continues after it
    /// waited for an approver.
    pub(crate) fn resumed(db: Db, hub: Hub, question: QuestionId, project: Option<ProjectId>) -> Self {
        Self {
            db,
            hub,
            question,
            project,
        }
    }

    pub fn question(&self) -> &QuestionId {
        &self.question
    }

    pub async fn emit(&mut self, event: Event) -> Result<Recorded<Event>, AuditError> {
        let now = db::now();
        // one transaction: a stream that sends this event, and a client that then asks where the
        // question stands, never see the row without the state, or the state without the row
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let pending = self.record_in(&mut tx, event, now).await?;
        let recorded = pending.commit(tx).await?;
        self.written(recorded.seq());
        Ok(recorded)
    }

    /// An `answer` and the `done` that ends it, in one transaction: the answer is in the audit only
    /// together with its ending, so a `done` that can't be written leaves no `answer` for a stream
    /// to send. `done` is given the `answer` row's `seq`, which it names. Returns the `done`.
    pub async fn emit_delivered(
        &mut self,
        answer: mercury_schema::api::Answer,
        done: impl FnOnce(u64) -> mercury_schema::events::Done,
    ) -> Result<Recorded<Event>, AuditError> {
        let now = db::now();
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let answered = self.record_in(&mut tx, Event::Answer(answer), now).await?;
        let ended = self
            .record_in(&mut tx, Event::Done(done(answered.seq())), now)
            .await?;
        let recorded = ended.commit(tx).await?;
        self.written(recorded.seq());
        Ok(recorded)
    }

    /// [`Emitter::emit`] inside `tx`, so that something else commits with the event: the held
    /// answer with its `pending_signoff`, a decision with the events it continues the question
    /// with. The caller commits through the last [`audit::Pending`], then calls
    /// [`Emitter::written`].
    pub(crate) async fn record_in(
        &mut self,
        tx: &mut Transaction<'_, Sqlite>,
        event: Event,
        now: DateTime<Utc>,
    ) -> Result<audit::Pending<Event>, AuditError> {
        if let Event::Project(picked) = &event {
            self.project = Some(picked.project.clone());
        }
        let pending = audit::record_in(tx, &self.question, self.project.as_ref(), event, now).await?;
        self.update(tx, pending.item(), now).await?;
        Ok(pending)
    }

    /// Wakes the streams once the row `seq` has committed.
    pub(crate) fn written(&self, seq: u64) {
        self.hub.written(seq);
    }

    /// The question's row as its events change it, inside the transaction that records the event.
    async fn update(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        event: &Event,
        now: DateTime<Utc>,
    ) -> Result<(), AuditError> {
        let id = self.question.as_str();
        let finished = db::time(now);
        let status = match event {
            Event::Project(picked) => {
                let project = picked.project.as_str();
                sqlx::query!("UPDATE questions SET project = ? WHERE id = ?", project, id)
                    .execute(&mut **tx)
                    .await?;
                return Ok(());
            }
            Event::Classified(classified) => {
                let class = crate::policy::class_name(classified.class);
                sqlx::query!("UPDATE questions SET class = ? WHERE id = ?", class, id)
                    .execute(&mut **tx)
                    .await?;
                return Ok(());
            }
            Event::Done(done) => {
                let mode = judge_mode_name(done.judge_mode);
                let cost = done.cost_usd.get();
                let duration = i64::try_from(done.duration_ms).unwrap_or(i64::MAX);
                sqlx::query!(
                    "UPDATE questions SET status = 'delivered', judge_mode = ?, cost_usd = ?, duration_ms = ?,
                     finished_at = ? WHERE id = ?",
                    mode,
                    cost,
                    duration,
                    finished,
                    id
                )
                .execute(&mut **tx)
                .await?;
                return Ok(());
            }
            // every other ending says what the question cost and which judge answered, and the
            // row says the same in the same transaction: the terminal row is the whole record
            Event::Refused(refused) => ("refused", refused.cost_usd, refused.judge_mode),
            Event::Clarify(clarify) => ("clarify", clarify.cost_usd, clarify.judge_mode),
            Event::Error(error) => ("failed", error.cost_usd, error.judge_mode),
            Event::PendingSignoff(_) => {
                sqlx::query!("UPDATE questions SET status = 'pending_signoff' WHERE id = ?", id)
                    .execute(&mut **tx)
                    .await?;
                return Ok(());
            }
            _ => return Ok(()),
        };
        let (status, cost, judge) = status;
        sqlx::query!(
            "UPDATE questions SET status = ?, finished_at = ? WHERE id = ?",
            status,
            finished,
            id
        )
        .execute(&mut **tx)
        .await?;
        let (cost, mode) = (cost.get(), judge.map(judge_mode_name));
        sqlx::query!(
            "UPDATE questions SET cost_usd = ?, judge_mode = ? WHERE id = ?",
            cost,
            mode,
            id
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

fn judge_mode_name(mode: JudgeMode) -> &'static str {
    match mode {
        JudgeMode::Jev => "jev",
        JudgeMode::Chat => "chat",
        JudgeMode::Rules => "rules",
    }
}

/// A question as the runner gets it. `text` is what the person typed, kept in memory only.
#[derive(Clone)]
pub struct Asked {
    pub id: QuestionId,
    pub caller: Caller,
    pub text: String,
    pub project: Option<ProjectId>,
    pub clarify: Option<ClarifyAnswer>,
}

/// Leaves the text out, so a stray `{:?}` doesn't log it.
impl std::fmt::Debug for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asked")
            .field("id", &self.id)
            .field("caller", &self.caller)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Answers a question after `accepted`: the orchestrator. It writes every event through the
/// emitter and ends with a terminal one.
#[async_trait]
pub trait Runner: Send + Sync + 'static {
    async fn run(&self, question: Asked, events: Emitter);
}

/// Until the orchestrator is wired in: every question ends with an `error`, so no client waits
/// for an answer that can't come.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotBuilt;

#[async_trait]
impl Runner for NotBuilt {
    async fn run(&self, _question: Asked, mut events: Emitter) {
        // nothing ran, so nothing was paid and no judge answered
        let error = Event::Error(EventError {
            code: ErrorCode::Internal,
            message: "Answering questions isn't available on this server yet.".to_owned(),
            cost_usd: Usd::default(),
            judge_mode: None,
        });
        if let Err(err) = events.emit(error).await {
            tracing::error!(error = %err, "cannot end a question");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The routes

/// What the routes share.
#[derive(Clone)]
pub struct Questions {
    pub db: Db,
    pub config: Arc<Config>,
    pub runner: Arc<dyn Runner>,
    pub hub: Hub,
    /// The rate limit on asking.
    pub limiter: RateLimiter,
    /// The web client's sessions. Until [`Questions::with_sessions`] sets them up, a session
    /// can't be started: no `MERCURY_PUBLIC_ORIGIN` is known.
    pub sessions: Sessions,
}

impl Questions {
    pub fn new(db: Db, config: Arc<Config>, runner: Arc<dyn Runner>) -> Self {
        let limits = &config.policies.limits;
        let limiter = RateLimiter::new(Rate {
            per_hour: limits.questions_per_hour,
            burst: limits.burst,
        });
        let sessions = Sessions::new(db.clone(), SessionConfig::default());
        Self {
            db,
            config,
            runner,
            hub: Hub::default(),
            limiter,
            sessions,
        }
    }

    /// With the web client's sessions set up as `config` says.
    #[must_use]
    pub fn with_sessions(mut self, config: SessionConfig) -> Self {
        self.sessions = Sessions::new(self.db.clone(), config);
        self
    }
}

impl FromRef<Questions> for Db {
    fn from_ref(state: &Questions) -> Db {
        state.db.clone()
    }
}

/// The routes of `/v1` besides the health check and the sessions' own. Asking is rate-limited per
/// token and its body capped; reading is authenticated on each request, by Bearer token or by the
/// web client's session cookie.
pub fn routes(state: Questions) -> Router {
    let sessions = state.sessions.clone();
    let limit = PerToken {
        db: state.db.clone(),
        limiter: state.limiter.clone(),
    };
    let ask = Router::new()
        .route("/v1/questions", post(ask))
        .layer(middleware::from_fn_with_state(limit, limits::per_token))
        .layer(middleware::from_fn(limits::body_cap))
        .with_state(state.clone());
    let feedback = Router::new()
        .route("/v1/questions/{id}/feedback", post(feedback))
        .layer(middleware::from_fn(limits::body_cap))
        .with_state(state.clone());
    Router::new()
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/me", get(me))
        .route("/v1/projects", get(projects))
        .route("/v1/questions/{id}", get(question))
        .route("/v1/questions/{id}/events", get(events))
        .with_state(state)
        .merge(ask)
        .merge(feedback)
        // outermost: the cookie becomes the caller before the rate limit or a handler asks who it is
        .layer(middleware::from_fn_with_state(sessions, session::resolve))
}

async fn openapi() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        mercury_schema::openapi::document_json(),
    )
        .into_response()
}

pub(crate) fn invalid(message: &str, hint: &str) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: ErrorCode::InvalidRequest,
        message: message.to_owned(),
        hint: hint.to_owned(),
        retry_after: None,
    }
}

/// The same answer for a question that isn't the caller's and one that doesn't exist.
fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: ErrorCode::InvalidRequest,
        message: "There is no such question.".to_owned(),
        hint: "Use the id that asking the question returned.".to_owned(),
        retry_after: None,
    }
}

fn internal(err: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %err, "a question route failed");
    ApiError::internal()
}

fn body<T>(parsed: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    parsed.map(|Json(body)| body).map_err(|_| {
        invalid(
            "The request body isn't the JSON this route takes.",
            "See /v1/openapi.json for the body.",
        )
    })
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let key = headers
        .get(IDEMPOTENCY_KEY)
        .and_then(|v| v.to_str().ok())
        .filter(|k| (1..=MAX_KEY_LEN).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b)));
    key.map(str::to_owned).ok_or_else(|| {
        invalid(
            "The request needs an Idempotency-Key header of 1 to 128 printable characters.",
            "Send a new random key with each question, and the same key when you retry it.",
        )
    })
}

/// The manifest a project's probe registered, if it registered.
async fn registered(
    db: &Db,
    project: &ProjectId,
) -> Result<Option<(ProbeStatus, String, ProbeManifest)>, ApiError> {
    let name = project.as_str();
    let row = sqlx::query!(
        "SELECT status, last_seen_at, manifest_json FROM probes WHERE project = ?",
        name
    )
    .fetch_optional(db.pool())
    .await
    .map_err(internal)?;
    let Some(row) = row else { return Ok(None) };
    let status = ProbeStatus::parse(&row.status).ok_or_else(|| internal("a probe has an unknown status"))?;
    let manifest = serde_json::from_str(&row.manifest_json)
        .map_err(|_| internal("a registered manifest doesn't parse"))?;
    Ok(Some((status, row.last_seen_at, manifest)))
}

fn granted(caller: &Caller, project: &ProjectId) -> bool {
    caller.projects.iter().any(|g| g.project == project.as_str())
}

async fn ask(
    State(state): State<Questions>,
    caller: Caller,
    headers: HeaderMap,
    parsed: Result<Json<NewQuestion>, JsonRejection>,
) -> Result<Response, ApiError> {
    let key = idempotency_key(&headers)?;
    let new = body(parsed)?;
    let channel = caller.channel();
    let id = start(&state, caller, channel, &key, new).await?;
    Ok((StatusCode::ACCEPTED, Json(Accepted::new(id))).into_response())
}

/// Who asked a question and where it stands, if it exists.
async fn asker_and_status(db: &Db, id: &QuestionId) -> Result<Option<(String, String)>, ApiError> {
    let id = id.as_str();
    let row = sqlx::query!("SELECT user_id, status FROM questions WHERE id = ?", id)
        .fetch_optional(db.pool())
        .await
        .map_err(internal)?;
    Ok(row.map(|r| (r.user_id, r.status)))
}

/// Whether `id` is the caller's own question that an approver rejected: it ended `refused` with
/// `rejected_by_approver`, after a rejection or after nobody decided within the day. One answer for
/// every other case, so a caller learns nothing of a question that isn't theirs.
async fn rejected_for(db: &Db, caller: &Caller, id: &QuestionId) -> Result<bool, ApiError> {
    let Some((asker, status)) = asker_and_status(db, id).await? else {
        return Ok(false);
    };
    if asker != caller.user_id || status != "refused" {
        return Ok(false);
    }
    let refusal: Option<Refusal> = last_event(db, id.as_str(), "refused").await?;
    Ok(refusal.is_some_and(|r| r.code == RefusalCode::RejectedByApprover))
}

/// Starts a question, for every way of asking one: `POST /v1/questions` and the tool `ask` at
/// `/mcp` both come here, so what an agent asks is checked exactly as what a person types. The
/// text's length, the clarify, the question it follows, the redaction, the key and the day's
/// budgets, then the `accepted` event (with `follows`, when it asks a rejected question again), the
/// run and its watcher. `key` is the idempotency key, already checked; `channel` says
/// where the question was asked from. Gives the question's id: the known one when the person used
/// the key in the last 24 hours.
pub(crate) async fn start(
    state: &Questions,
    caller: Caller,
    channel: Channel,
    key: &str,
    new: NewQuestion,
) -> Result<QuestionId, ApiError> {
    let text = new.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(
            "The question must be 1 to 2,000 characters.",
            "Ask a shorter question.",
        ));
    }
    if let Some(clarify) = &new.clarify {
        let row = asker_and_status(&state.db, &clarify.of).await?;
        if !row.is_some_and(|(asker, status)| asker == caller.user_id && status == "clarify") {
            return Err(invalid(
                "The question to clarify isn't yours, or didn't end with a clarify.",
                "Answer the clarify of your own question.",
            ));
        }
    }
    if let Some(follows) = &new.follows
        && !rejected_for(&state.db, &caller, follows).await?
    {
        return Err(invalid(FOLLOWS_REFUSED.0, FOLLOWS_REFUSED.1));
    }

    // the global rules, and the project's when it is granted and known
    let mut rules = state.config.policies.global.redact.clone();
    if let Some(project) = new.project.as_ref().filter(|p| granted(&caller, p)) {
        let registered = registered(&state.db, project).await?.map(|(_, _, m)| m);
        if let Some(manifest) = config::manifest_of(&state.config.projects, project, registered.as_ref()) {
            rules.extend(manifest.redact);
        }
    }
    let redactor = Redactor::new(&rules).map_err(internal)?;
    let text_redacted = redactor.apply(&text).into_owned();

    let now = db::now();
    let (id, fresh) = match remember(state, &caller, channel, key, &new, &text_redacted, now).await? {
        Remembered::Known(id) => (id, false),
        Remembered::New(id) => (id, true),
        Remembered::Spent(cap) => return Err(over_budget(&state.db, &caller, cap, now).await),
    };
    if fresh {
        let mut emitter = Emitter::new(state.db.clone(), state.hub.clone(), id.clone());
        // a question asked again names the rejected one in its first row, which links the two
        let accepted = Accepted {
            id: id.clone(),
            follows: new.follows.clone(),
        };
        if let Err(err) = emitter.emit(Event::Accepted(accepted)).await {
            forget(&state.db, &caller, key, &id).await;
            return Err(internal(err));
        }
        let asked = Asked {
            id: id.clone(),
            caller,
            text,
            project: new.project,
            clarify: new.clarify,
        };
        let runner = state.runner.clone();
        let watcher = emitter.clone();
        let run = tokio::spawn(async move { runner.run(asked, emitter).await });
        // the run is watched: one that panics, or ends without a terminal event, is ended here
        tokio::spawn(async move {
            if run.await.is_err() {
                tracing::error!("a question's run stopped before its end");
            }
            end_if_open(watcher, "The server could not finish the question.").await;
        });
    }
    Ok(id)
}

/// A question whose `accepted` couldn't be written never started: its row and its key go, so a
/// retry with the same key asks afresh. When even that fails, its row says `failed`.
async fn forget(db: &Db, caller: &Caller, key: &str, id: &QuestionId) {
    let (user, qid) = (caller.user_id.as_str(), id.as_str());
    let forgotten: Result<(), sqlx::Error> = async {
        let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query!(
            "DELETE FROM idempotency_keys WHERE user_id = ? AND key = ?",
            user,
            key
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM questions WHERE id = ?", qid)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    if let Err(err) = forgotten {
        tracing::error!(error = %err, "a question that never started can't be dropped");
        let _ = sqlx::query!("UPDATE questions SET status = 'failed' WHERE id = ?", qid)
            .execute(db.pool())
            .await;
    }
}

/// Ends a question that is still running with `error` `internal`, so its streams don't wait for an
/// end that can't come: a run that stopped early (a panic, a crash, a restart of the core). The
/// error says what the question cost and which judge answered, as its row has kept them while it
/// ran. When even that row can't be written, the question's row says `failed`, which ends the
/// streams too.
async fn end_if_open(mut events: Emitter, message: &str) {
    let qid = events.question.as_str().to_owned();
    let row = sqlx::query!(
        "SELECT status, project, cost_usd, judge_mode FROM questions WHERE id = ?",
        qid
    )
    .fetch_optional(events.db.pool())
    .await;
    let row = match row {
        Ok(Some(row)) if row.status == "running" => row,
        Ok(_) => return,
        Err(err) => {
            tracing::error!(error = %err, "cannot read a question that may still be running");
            return;
        }
    };
    events.project = row.project.as_deref().and_then(|p| ProjectId::parse(p).ok());
    // only a negative cost, which nothing writes, isn't one; the ending still has to be written
    let cost_usd = Usd::new(row.cost_usd).unwrap_or_else(|| {
        tracing::error!("a question that stopped has a cost that isn't one");
        Usd::default()
    });
    let error = Event::Error(EventError {
        code: ErrorCode::Internal,
        message: message.to_owned(),
        cost_usd,
        judge_mode: row.judge_mode.as_deref().and_then(parse_name::<JudgeMode>),
    });
    if let Err(err) = events.emit(error).await {
        tracing::error!(error = %err, "a question that stopped can't be ended with an error event");
        let finished = db::time(db::now());
        let failed = sqlx::query!(
            "UPDATE questions SET status = 'failed', finished_at = COALESCE(finished_at, ?) WHERE id = ?",
            finished,
            qid
        )
        .execute(events.db.pool())
        .await;
        if let Err(err) = failed {
            tracing::error!(error = %err, "a question that stopped can't be marked failed");
        }
        events.hub.wake();
    }
}

/// At start: the questions a previous run of the core left running end with `error` `internal`,
/// since nothing answers them now. Gives how many there were.
pub async fn end_orphans(db: &Db) -> Result<usize, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar!("SELECT id FROM questions WHERE status = 'running'")
        .fetch_all(db.pool())
        .await?;
    let hub = Hub::default();
    for id in &ids {
        if let Ok(id) = QuestionId::parse(id) {
            let events = Emitter::new(db.clone(), hub.clone(), id);
            end_if_open(events, "The server restarted before the question was finished.").await;
        }
    }
    Ok(ids.len())
}

/// What a key stands for once [`remember`] has looked.
enum Remembered {
    /// The question asked with it in the last 24 hours.
    Known(QuestionId),
    New(QuestionId),
    /// No question: the person's or everyone's budget for the day is spent.
    Spent(Cap),
}

/// The question for this key: the one the person asked with it in the last 24 hours, or a new
/// one from `channel`, written with the key in one transaction while the day's budgets have room.
async fn remember(
    state: &Questions,
    caller: &Caller,
    channel: Channel,
    key: &str,
    new: &NewQuestion,
    text_redacted: &str,
    now: DateTime<Utc>,
) -> Result<Remembered, ApiError> {
    let (db, budgets) = (&state.db, &state.config.models.budgets);
    let created = db::time(now);
    let expired = db::time(now - KEY_TTL);
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await.map_err(internal)?;
    sqlx::query!("DELETE FROM idempotency_keys WHERE created_at < ?", expired)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    let user = caller.user_id.as_str();
    let known: Option<String> = sqlx::query_scalar!(
        "SELECT question_id FROM idempotency_keys WHERE user_id = ? AND key = ?",
        user,
        key
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;
    if let Some(known) = known {
        tx.commit().await.map_err(internal)?;
        return QuestionId::parse(&known).map(Remembered::Known).map_err(internal);
    }
    // fails closed: a budget that can't be read lets no question start
    if let Some(cap) = budget::spent(&mut tx, budgets, user, now)
        .await
        .map_err(internal)?
    {
        tx.commit().await.map_err(internal)?;
        return Ok(Remembered::Spent(cap));
    }
    let id = new_question_id(now).map_err(internal)?;
    let qid = id.as_str();
    let token = caller.token.public_id.as_str();
    let channel = channel.as_str();
    let project = new.project.as_ref().map(ProjectId::as_str);
    let clarify_of = new.clarify.as_ref().map(|c| c.of.as_str());
    sqlx::query!(
        "INSERT INTO questions (id, user_id, token_public_id, channel, project, status, clarify_of,
                                text_redacted, created_at)
         VALUES (?, ?, ?, ?, ?, 'running', ?, ?, ?)",
        qid,
        user,
        token,
        channel,
        project,
        clarify_of,
        text_redacted,
        created
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    sqlx::query!(
        "INSERT INTO idempotency_keys (key, user_id, question_id, created_at) VALUES (?, ?, ?, ?)",
        key,
        user,
        qid,
        created
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Remembered::New(id))
}

/// `budget_exceeded` for a question that can't start, once its audit row is written: no audit,
/// no answer.
async fn over_budget(db: &Db, caller: &Caller, cap: Cap, now: DateTime<Utc>) -> ApiError {
    let payload = serde_json::json!({
        "token": caller.token.public_id.as_str(),
        "cap": cap.name(),
        "day": budget::day(now),
    });
    if let Err(err) = audit::action(db, "budget_exceeded", None, &payload, now).await {
        return internal(err);
    }
    let seconds = u64::try_from(budget::until_tomorrow(now).num_seconds())
        .unwrap_or(1)
        .max(1);
    let message = match cap {
        Cap::User => "You have used up today's budget for questions.",
        Cap::Global => "Mercury has used up today's budget for questions.",
    };
    ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: ErrorCode::BudgetExceeded,
        message: message.to_owned(),
        hint: "Ask again tomorrow: the budget starts again at midnight UTC.".to_owned(),
        retry_after: Some(seconds),
    }
}

fn new_question_id(now: DateTime<Utc>) -> Result<QuestionId, String> {
    let mut random = [0; 10];
    getrandom::fill(&mut random).map_err(|err| err.to_string())?;
    let millis = u64::try_from(now.timestamp_millis()).unwrap_or(0);
    QuestionId::parse(&format!("q_{}", crate::admin::encode_ulid(millis, random)))
        .map_err(|err| err.to_string())
}

/// The caller's own question, or 404.
async fn owned(db: &Db, caller: &Caller, id: &str) -> Result<QuestionId, ApiError> {
    let id = QuestionId::parse(id).map_err(|_| not_found())?;
    let qid = id.as_str();
    let owner: Option<String> = sqlx::query_scalar!("SELECT user_id FROM questions WHERE id = ?", qid)
        .fetch_optional(db.pool())
        .await
        .map_err(internal)?;
    match owner {
        Some(owner) if owner == caller.user_id => Ok(id),
        _ => Err(not_found()),
    }
}

async fn events(
    State(state): State<Questions>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = owned(&state.db, &caller, &id).await?;
    let after = match headers.get(LAST_EVENT_ID) {
        None => 0,
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .ok_or_else(|| {
                invalid(
                    "Last-Event-ID must be an event's id, a number.",
                    "Send the last id you received.",
                )
            })?,
    };
    let rx = rows(&state, id, after);
    let stream = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|row| (Ok::<_, Infallible>(row.sse()), rx))
    });
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE));
    Ok((
        [
            (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ],
        sse,
    )
        .into_response())
}

/// One of a question's events, as its audit row holds it.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub seq: u64,
    /// The event's name: `accepted`, `classified`, `answer`…
    pub kind: String,
    pub payload: Option<String>,
}

impl Row {
    /// The row as its stream sends it: the event's name, its `seq` as the id, its data.
    fn sse(&self) -> SseEvent {
        SseEvent::default()
            .event(self.kind.as_str())
            .id(self.seq.to_string())
            .data(self.payload.as_deref().unwrap_or("null"))
    }

    /// Whether the event ends a question's stream.
    pub fn is_terminal(&self) -> bool {
        TERMINAL.contains(&self.kind.as_str())
    }
}

/// The question's events after `after`, then each new one as it is written, up to its end: what
/// its stream sends, for every way of following a question. The channel closes at the end, or
/// once nobody reads it.
pub(crate) fn rows(state: &Questions, id: QuestionId, after: u64) -> mpsc::Receiver<Row> {
    // subscribed before the first read, so no event falls between the read and the wait
    let woken = state.hub.subscribe();
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(follow(state.db.clone(), id, after, woken, tx));
    rx
}

/// An ending as the core writes it today. A row written before the endings carried them lacks a
/// field: `composer` on a `done`; `cost_usd` and `judge_mode` on a `refused`, a `clarify` and an
/// `error`. A client that reads the stream strictly would stop at such a row, so the replay fills
/// what is missing from the question's own row, which holds the same facts: its cost, its judge,
/// and a prompt version that says whether the template wrote the answer. A row that has its
/// fields is sent exactly as it was written, and the audit keeps what it was written as.
async fn upgraded(db: &Db, id: &str, mut row: Row) -> Row {
    let missing: &[&str] = match row.kind.as_str() {
        "done" => &["composer"],
        "refused" | "clarify" | "error" => &["cost_usd", "judge_mode"],
        _ => return row,
    };
    let Some(Ok(serde_json::Value::Object(mut data))) = row
        .payload
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
    else {
        return row;
    };
    if missing.iter().all(|key| data.contains_key(*key)) {
        return row;
    }
    let held = sqlx::query!(
        "SELECT cost_usd, judge_mode, prompt_version FROM questions WHERE id = ?",
        id
    )
    .fetch_optional(db.pool())
    .await;
    let Ok(Some(held)) = held else {
        return row;
    };
    if row.kind == "done" {
        let composer = if held.prompt_version.as_deref() == Some("template") {
            "template"
        } else {
            "model"
        };
        data.entry("composer").or_insert_with(|| composer.into());
    } else {
        data.entry("cost_usd").or_insert_with(|| held.cost_usd.into());
        data.entry("judge_mode").or_insert_with(|| held.judge_mode.into());
    }
    row.payload = Some(serde_json::Value::Object(data).to_string());
    row
}

/// Sends the question's rows after `after`, then each new one, until a terminal event, until the
/// question has ended without one it could send, or until the reader goes away.
async fn follow(
    db: Db,
    id: QuestionId,
    mut after: u64,
    mut woken: watch::Receiver<u64>,
    tx: mpsc::Sender<Row>,
) {
    let qid = id.as_str().to_owned();
    let mut ended = false;
    loop {
        let from = i64::try_from(after).unwrap_or(i64::MAX);
        let rows = match sqlx::query!(
            r#"SELECT seq AS "seq!", type AS kind, payload_json FROM audit_events
               WHERE question_id = ? AND seq > ? ORDER BY seq"#,
            qid,
            from
        )
        .fetch_all(db.pool())
        .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!(error = %err, "a question's stream can't read its events");
                return;
            }
        };
        let count = rows.len();
        for (i, row) in rows.into_iter().enumerate() {
            let row = Row {
                seq: u64::try_from(row.seq).unwrap_or(0),
                kind: row.kind,
                payload: row.payload_json,
            };
            let row = upgraded(&db, &qid, row).await;
            let (seq, terminal, held) = (row.seq, row.is_terminal(), row.kind == "pending_signoff");
            if tx.send(row).await.is_err() {
                return;
            }
            after = seq;
            // a held answer ends the stream only while nothing came after it: a replay after the
            // decision goes on to the answer or the refusal
            let last = i + 1 == count;
            if terminal && (!held || last) {
                return;
            }
        }
        if ended {
            return;
        }
        // a question's rows are written before its row says it ended, so once it has, one more
        // read sends whatever came last, and the stream ends
        match has_ended(&db, &qid).await {
            Ok(true) => {
                ended = true;
                continue;
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!(error = %err, "a question's stream can't read its question");
                return;
            }
        }
        tokio::select! {
            changed = woken.changed() => if changed.is_err() { return },
            () = tx.closed() => return,
            () = tokio::time::sleep(RECHECK) => {}
        }
    }
}

/// Whether the question has ended: its row is gone, or says neither running nor waiting for a
/// sign-off.
async fn has_ended(db: &Db, id: &str) -> Result<bool, sqlx::Error> {
    let status: Option<String> = sqlx::query_scalar!("SELECT status FROM questions WHERE id = ?", id)
        .fetch_optional(db.pool())
        .await?;
    Ok(status.is_none_or(|s| s != "running" && s != "pending_signoff"))
}

/// The data of the question's last event of this type.
pub(crate) async fn last_event<T: DeserializeOwned>(
    db: &Db,
    id: &str,
    kind: &str,
) -> Result<Option<T>, ApiError> {
    let payload: Option<Option<String>> = sqlx::query_scalar!(
        "SELECT payload_json FROM audit_events WHERE question_id = ? AND type = ? ORDER BY seq DESC LIMIT 1",
        id,
        kind
    )
    .fetch_optional(db.pool())
    .await
    .map_err(internal)?;
    payload
        .flatten()
        .map(|p| serde_json::from_str(&p).map_err(|_| internal("an event's payload doesn't parse")))
        .transpose()
}

fn parse_name<T: DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

async fn question(
    State(state): State<Questions>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<QuestionState>, ApiError> {
    let id = owned(&state.db, &caller, &id).await?;
    state_of(&state.db, &id).await.map(Json)
}

/// Where a question stands, as `GET /v1/questions/{id}` says: its row, and the answer, the refusal
/// and the audit id from its events. Nothing of a draft: an answer is here once its `answer` event
/// is, after the output checks.
pub(crate) async fn state_of(db: &Db, id: &QuestionId) -> Result<QuestionState, ApiError> {
    let qid = id.as_str();
    let row = sqlx::query!(
        "SELECT status, project, class, judge_mode, cost_usd, duration_ms FROM questions WHERE id = ?",
        qid
    )
    .fetch_one(db.pool())
    .await
    .map_err(internal)?;
    // only the audit id of the ending: a `done` written before it named its composer still opens
    let done: Option<serde_json::Value> = last_event(db, qid, "done").await?;
    let audit_id = done.and_then(|done| done.get("audit_id").and_then(serde_json::Value::as_u64));
    Ok(QuestionState {
        id: id.clone(),
        status: parse_name::<QuestionStatus>(&row.status)
            .ok_or_else(|| internal("a question has an unknown status"))?,
        project: row
            .project
            .as_deref()
            .map(ProjectId::parse)
            .transpose()
            .map_err(internal)?,
        class: row.class.as_deref().and_then(parse_name::<QuestionClass>),
        answer: last_event::<Answer>(db, qid, "answer").await?,
        refusal: last_event::<Refusal>(db, qid, "refused").await?,
        judge_mode: row.judge_mode.as_deref().and_then(parse_name::<JudgeMode>),
        cost_usd: Usd::new(row.cost_usd).ok_or_else(|| internal("a question's cost is negative"))?,
        duration_ms: row.duration_ms.and_then(|d| u64::try_from(d).ok()),
        audit_id,
    })
}

async fn feedback(
    State(state): State<Questions>,
    caller: Caller,
    Path(id): Path<String>,
    parsed: Result<Json<Feedback>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let id = owned(&state.db, &caller, &id).await?;
    let feedback = body(parsed)?;
    let qid = id.as_str();
    let created = db::time(db::now());
    // an audit row with the question's id in its data, not in its question column: the question's
    // stream replays its rows, and feedback isn't one of its events
    let payload = serde_json::json!({
        "question_id": qid,
        "helpful": feedback.helpful,
        "needed_dev": feedback.needed_dev,
    });
    // the feedback and its row are one transaction: a row that can't be written is a `500` with no
    // feedback kept, and sending it again is safe
    let mut tx = state
        .db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(internal)?;
    sqlx::query!(
        "INSERT INTO feedback (question_id, helpful, needed_dev, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (question_id) DO UPDATE SET helpful = excluded.helpful,
           needed_dev = excluded.needed_dev, created_at = excluded.created_at",
        qid,
        feedback.helpful,
        feedback.needed_dev,
        created
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    audit::action_in(&mut tx, "feedback", None, &payload, db::now())
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn me(caller: Caller) -> Result<Json<Me>, ApiError> {
    let corrupt = |_| internal("a stored user or token doesn't have the expected shape");
    Ok(Json(Me {
        user: MeUser {
            id: UserId::parse(&caller.user_id).map_err(corrupt)?,
            email: Email::parse(&caller.email).map_err(corrupt)?,
            display_name: caller.display_name.clone(),
            role: caller.role,
        },
        projects: caller
            .projects
            .iter()
            .map(|g| {
                Ok(ApiGrant {
                    project: ProjectId::parse(&g.project).map_err(corrupt)?,
                    approver: g.approver,
                })
            })
            .collect::<Result<_, ApiError>>()?,
        token: MeToken {
            public_id: TokenPublicId::parse(&caller.token.public_id).map_err(corrupt)?,
            expires_at: caller.token.expires_at,
        },
    }))
}

async fn projects(State(state): State<Questions>, caller: Caller) -> Result<Json<ProjectList>, ApiError> {
    project_list(&state, &caller).await.map(Json)
}

/// The projects the caller may ask about, as `GET /v1/projects` lists them: each granted project
/// but `portfolio` that has a manifest, with its modules, its classes for the caller's role and its
/// probe's state.
pub(crate) async fn project_list(state: &Questions, caller: &Caller) -> Result<ProjectList, ApiError> {
    let now = db::now();
    let mut projects = Vec::new();
    for grant in caller.projects.iter().filter(|g| g.project != PORTFOLIO) {
        let Ok(project) = ProjectId::parse(&grant.project) else {
            continue;
        };
        let registered = registered(&state.db, &project).await?;
        let manifest_ref = registered.as_ref().map(|(_, _, m)| m);
        let Some(manifest) = config::manifest_of(&state.config.projects, &project, manifest_ref) else {
            continue;
        };
        let mut modules = Vec::new();
        for module in manifest.modules.keys().chain(manifest.module_labels.keys()) {
            if !modules.contains(module) {
                modules.push(module.clone());
            }
        }
        let classes = EffectivePolicy::for_project(&state.config.policies, caller.role, &manifest).allowed;
        let probe = match &registered {
            Some((status, last_seen, _)) => {
                let as_of =
                    db::parse_time(last_seen).ok_or_else(|| internal("a probe's heartbeat isn't a time"))?;
                let status = match status.at(last_seen, now) {
                    ProbeStatus::Healthy => ApiProbeStatus::Healthy,
                    ProbeStatus::Stale => ApiProbeStatus::Stale,
                    ProbeStatus::Quarantined => ApiProbeStatus::Quarantined,
                };
                Some(ProbeState { status, as_of })
            }
            None => None,
        };
        projects.push(ProjectSummary {
            project,
            display_name: manifest.display_name.clone(),
            modules,
            classes,
            probe,
        });
    }
    Ok(ProjectList { projects })
}
