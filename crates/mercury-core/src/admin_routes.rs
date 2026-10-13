//! `GET /v1/admin/overview`, `GET /v1/admin/people` and `GET /v1/admin/repositories`: what the web
//! client's admin panel shows, an admin's alone. Anyone else is refused before anything is read,
//! whichever client asks.
//!
//! They change nothing. People, roles, grants, tokens and a probe's quarantine change through the
//! admin commands on the server, and the settings shown (the model backends, the budgets, the
//! thresholds) through the files there. Each read is one audit row without a question,
//! `admin_viewed`, written before the body is sent: no data without its row.
//!
//! Aggregates and metadata only. No query here reads a token's secret, a question's text, an
//! answer, a fact or a draft. A stored row that doesn't have its shape fails the read with
//! `internal`; it is never guessed at.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use chrono::{DateTime, NaiveTime, TimeDelta, Timelike, Utc};
use mercury_schema::admin::{
    AdminAlert, AdminGrant, AdminOverview, AdminPeople, AdminPerson, AdminProbe, AdminRepositories,
    AdminRepository, AdminToken, AlertKind, Bucket, BudgetSettings, ChannelCount, CheckLine, ClassCount,
    Durations, GrantClass, JudgeCounts, ModelRole, ModelRoleLine, OutcomeCounts, OverviewWindow,
    ProjectCount, RefusalCount, RepositoryPerson, SeriesPoint, Settings, SignoffCounts, SpendToday, Spender,
    TokenState,
};
use mercury_schema::api::{ErrorCode, ProbeStatus as ApiProbeStatus, QuestionStatus, RefusalCode, Usd};
use mercury_schema::config::{ChatRole, JudgeRole};
use mercury_schema::facts::SchemaId;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{
    AccessLimit, AccessMode, Channel, Email, JudgeMode, ModuleName, ProjectId, QuestionClass, TokenPublicId,
    ToolName, UserId,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::admin::{self, Classes};
use crate::alerts::{self, Alert, Degraded};
use crate::audit;
use crate::audit_routes::admin_only;
use crate::config::Config;
use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::{Caller, parse_role};
use crate::orchestrator::budget::{self, EVERYONE};
use crate::probes::ProbeStatus;
use crate::questions::Questions;
use crate::session;
use crate::status::{self, Outcome};

/// How far back the people's and the repositories' questions are counted.
pub const RECENT: TimeDelta = TimeDelta::days(30);

/// A sum of amounts this little below zero is the floats' rounding, read as zero; one further below
/// is a broken row.
const ROUNDING: f64 = 1e-9;

/// The overview's one parameter.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowParam {
    window: Option<String>,
}

/// People and repositories take no parameter.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParam {}

pub fn routes(state: Questions) -> Router {
    let sessions = state.sessions.clone();
    Router::new()
        .route("/v1/admin/overview", get(overview))
        .route("/v1/admin/people", get(people))
        .route("/v1/admin/repositories", get(repositories))
        .with_state(state)
        // the cookie becomes the caller, as for the audit's routes
        .layer(middleware::from_fn_with_state(sessions, session::resolve))
}

// ---------------------------------------------------------------------------------------------
// The routes

/// The admin panel is an admin's: anyone else is refused before anything is read.
fn admins_only(caller: &Caller) -> Result<(), ApiError> {
    admin_only(
        caller,
        "Only an admin reads the admin panel.",
        "Your own questions and projects are in the web client; ask an admin for anything else.",
    )
}

fn invalid(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: ErrorCode::InvalidRequest,
        message: message.to_owned(),
        hint: "See /v1/openapi.json for the admin routes and their parameters.".to_owned(),
        retry_after: None,
    }
}

fn params<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(params)| params)
        .map_err(|_| invalid("The query has a parameter this route doesn't take, or one given twice."))
}

fn failed(err: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %err, "an admin panel route failed");
    ApiError::internal()
}

/// A stored row without the shape it should have: the read fails rather than guess.
fn corrupt(what: &'static str) -> ApiError {
    tracing::error!(
        row = what,
        "an admin panel route found a stored row it can't read"
    );
    ApiError::internal()
}

/// The read's audit row, written before its body is sent: the section, the overview's window, and
/// the token that read, so an unexpected reader shows in the audit.
async fn viewed(
    db: &Db,
    caller: &Caller,
    section: &str,
    window: Option<OverviewWindow>,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let mut payload = json!({ "section": section, "token": caller.token.public_id });
    if let Some(window) = window {
        payload["window"] = json!(window);
    }
    // no project on the row: reading the panel is an admin's act, not something that happened in
    // a project
    audit::action(db, "admin_viewed", None, &payload, now)
        .await
        .map_err(failed)?;
    Ok(())
}

/// The body, kept by no cache on the way.
fn sent<T: Serialize>(body: T) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

async fn overview(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<WindowParam>, QueryRejection>,
) -> Result<Response, ApiError> {
    admins_only(&caller)?;
    let params = params(query)?;
    let window = match params.window.as_deref() {
        None => OverviewWindow::default(),
        Some(raw) => named(raw).ok_or_else(|| invalid("The parameter `window` is 24h, 7d or 30d."))?,
    };
    let now = db::now();
    let body = read_overview(&state.db, &state.config, window, now).await?;
    viewed(&state.db, &caller, "overview", Some(window), now).await?;
    Ok(sent(body))
}

async fn people(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<NoParam>, QueryRejection>,
) -> Result<Response, ApiError> {
    admins_only(&caller)?;
    params(query)?;
    let now = db::now();
    let body = read_people(&state.db, &state.config, now).await?;
    viewed(&state.db, &caller, "people", None, now).await?;
    Ok(sent(body))
}

async fn repositories(
    State(state): State<Questions>,
    caller: Caller,
    query: Result<Query<NoParam>, QueryRejection>,
) -> Result<Response, ApiError> {
    admins_only(&caller)?;
    params(query)?;
    let now = db::now();
    let body = read_repositories(&state.db, &state.config, now).await?;
    viewed(&state.db, &caller, "repositories", None, now).await?;
    Ok(sent(body))
}

// ---------------------------------------------------------------------------------------------
// Reading stored values

/// A wire name of an enum, `delivered` say.
fn named<T: DeserializeOwned>(raw: &str) -> Option<T> {
    serde_json::from_value(Value::String(raw.to_owned())).ok()
}

/// The stored name of an enum, or the read fails.
fn stored<T: DeserializeOwned>(raw: &str, what: &'static str) -> Result<T, ApiError> {
    named(raw).ok_or_else(|| corrupt(what))
}

/// An enum's wire name, to order equal counts the same way every time.
fn wire_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => String::new(),
    }
}

fn time(text: &str, what: &'static str) -> Result<DateTime<Utc>, ApiError> {
    db::parse_time(text).ok_or_else(|| corrupt(what))
}

fn maybe_time(text: Option<&str>, what: &'static str) -> Result<Option<DateTime<Utc>>, ApiError> {
    text.map(|text| time(text, what)).transpose()
}

fn project(text: &str) -> Result<ProjectId, ApiError> {
    ProjectId::parse(text).map_err(|_| corrupt("a project id"))
}

/// An amount in dollars: never negative, the floats' rounding below zero read as zero.
fn usd(amount: f64) -> Result<Usd, ApiError> {
    if !amount.is_finite() || amount < -ROUNDING {
        return Err(corrupt("an amount in dollars"));
    }
    Usd::new(amount.max(0.0)).ok_or_else(|| corrupt("an amount in dollars"))
}

/// A count SQLite gave; it is never negative.
fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// The overview

/// The window's start, how it is cut, and into how many buckets. The last bucket is the request's
/// hour or day and the first starts at the window's start, so every question of the window is in
/// exactly one bucket: the 24 hours ending with the current one, or the 7 or 30 UTC days ending
/// with today.
fn span(window: OverviewWindow, now: DateTime<Utc>) -> (DateTime<Utc>, Bucket, usize) {
    let midnight = now.date_naive().and_time(NaiveTime::MIN).and_utc();
    match window {
        OverviewWindow::Day => {
            let hour = midnight + TimeDelta::hours(i64::from(now.hour()));
            (hour - TimeDelta::hours(23), Bucket::Hour, 24)
        }
        OverviewWindow::Week => (midnight - TimeDelta::days(6), Bucket::Day, 7),
        OverviewWindow::Month => (midnight - TimeDelta::days(29), Bucket::Day, 30),
    }
}

fn step(bucket: Bucket) -> TimeDelta {
    match bucket {
        Bucket::Hour => TimeDelta::hours(1),
        Bucket::Day => TimeDelta::days(1),
    }
}

fn add(counts: &mut OutcomeCounts, outcome: Outcome) {
    let slot = match outcome {
        Outcome::Answered => &mut counts.answered,
        Outcome::Refused => &mut counts.refused,
        Outcome::Degraded => &mut counts.degraded,
        Outcome::Held => &mut counts.held,
        Outcome::Other => &mut counts.other,
    };
    *slot += 1;
}

fn total(counts: &OutcomeCounts) -> u64 {
    counts.answered + counts.refused + counts.degraded + counts.held + counts.other
}

/// The nearest-rank percentiles and the longest of `sorted`, which is in ascending order.
fn durations(sorted: &[u64]) -> Durations {
    let rank = |percent: u64| -> Option<u64> {
        let n = u64::try_from(sorted.len()).ok()?;
        let rank = (percent * n).div_ceil(100).max(1);
        sorted.get(usize::try_from(rank - 1).ok()?).copied()
    };
    Durations {
        p50: rank(50),
        p95: rank(95),
        max: sorted.last().copied(),
    }
}

/// What `GET /v1/admin/overview` shows for `window` at `now`. The window's questions are the ones
/// asked in it; the judges count the questions that ended in it, as `admin status` counts them,
/// and the durations those that finished in it without a hold for a sign-off.
pub(crate) async fn read_overview(
    db: &Db,
    config: &Config,
    window: OverviewWindow,
    now: DateTime<Utc>,
) -> Result<AdminOverview, ApiError> {
    let (since, bucket, buckets) = span(window, now);
    let seconds = step(bucket).num_seconds();
    let from = db::time(since);

    let rows = sqlx::query!(
        r#"SELECT q.user_id AS "user_id!", q.channel AS "channel!", q.project AS "project?",
                  q.class AS "class?", q.status AS "status!", q.judge_mode AS "judge_mode?",
                  q.cost_usd AS "cost_usd!: f64", q.created_at AS "created_at!",
                  EXISTS (SELECT 1 FROM audit_events a
                          WHERE a.question_id = q.id AND a.type = 'pending_signoff') AS "held!: bool"
           FROM questions q WHERE q.created_at >= ? ORDER BY q.created_at, q.id"#,
        from
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?;

    let mut totals = OutcomeCounts::default();
    let mut series = vec![OutcomeCounts::default(); buckets];
    let mut channels: Vec<ChannelCount> = Channel::ALL
        .into_iter()
        .map(|channel| ChannelCount {
            channel,
            counts: OutcomeCounts::default(),
        })
        .collect();
    let mut by_project: BTreeMap<Option<String>, (OutcomeCounts, f64)> = BTreeMap::new();
    let mut classes: Vec<ClassCount> = Vec::new();
    let mut cost = 0.0;
    let mut askers = HashSet::new();
    for row in rows {
        let _: QuestionStatus = stored(&row.status, "a question's status")?;
        if let Some(mode) = &row.judge_mode {
            let _: JudgeMode = stored(mode, "a question's judge")?;
        }
        let outcome = Outcome::of(&row.status, row.judge_mode.as_deref(), row.held);
        let channel: Channel = stored(&row.channel, "a question's channel")?;
        let asked = time(&row.created_at, "a question's time")?;
        let at = usize::try_from((asked - since).num_seconds() / seconds)
            .unwrap_or(0)
            .min(buckets - 1);
        add(&mut totals, outcome);
        add(&mut series[at], outcome);
        if let Some(slot) = channels.iter_mut().find(|c| c.channel == channel) {
            add(&mut slot.counts, outcome);
        }
        if let Some(name) = &row.project {
            project(name)?;
        }
        let (counts, spent) = by_project.entry(row.project).or_default();
        add(counts, outcome);
        *spent += row.cost_usd;
        if let Some(class) = &row.class {
            let class: QuestionClass = stored(class, "a question's class")?;
            match classes.iter_mut().find(|c| c.class == class) {
                Some(slot) => slot.count += 1,
                None => classes.push(ClassCount { class, count: 1 }),
            }
        }
        cost += row.cost_usd;
        askers.insert(row.user_id);
    }
    classes.sort_by_key(|c| (Reverse(c.count), wire_name(&c.class)));

    let mut projects = Vec::with_capacity(by_project.len());
    for (name, (counts, spent)) in by_project {
        projects.push(ProjectCount {
            project: name.as_deref().map(project).transpose()?,
            counts,
            cost_usd: usd(spent)?,
        });
    }
    // the most asked first; among equals by id, and the questions no project took last
    projects.sort_by_key(|p| (Reverse(total(&p.counts)), p.project.is_none()));

    let series = series
        .into_iter()
        .zip(0_i32..)
        .map(|(counts, n)| SeriesPoint {
            start: since + step(bucket) * n,
            counts,
        })
        .collect();

    Ok(AdminOverview {
        window,
        since,
        until: now,
        bucket,
        totals,
        series,
        channels,
        projects,
        classes,
        refusals: refusals(db, &from).await?,
        judge: judges(db, since).await?,
        duration_ms: finished(db, &from).await?,
        cost_usd: usd(cost)?,
        askers: u64::try_from(askers.len()).unwrap_or(u64::MAX),
        signoffs: signoffs(db, &from, now).await?,
        spend: spend(db, config, now).await?,
        alerts: alerts_now(db, config, now).await?,
        settings: settings(config),
    })
}

/// Why the window's questions were refused, from their `refused` rows: the most first. A held
/// question an approver rejected counts here with `rejected_by_approver`, though its outcome is
/// `held`. A row whose payload retention cleared says nothing and is left out.
async fn refusals(db: &Db, from: &str) -> Result<Vec<RefusalCount>, ApiError> {
    #[derive(Deserialize)]
    struct Code {
        code: RefusalCode,
    }
    let payloads = sqlx::query_scalar!(
        "SELECT a.payload_json FROM audit_events a JOIN questions q ON q.id = a.question_id
         WHERE a.type = 'refused' AND q.created_at >= ? ORDER BY a.seq",
        from
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?;
    let mut refusals: Vec<RefusalCount> = Vec::new();
    for payload in payloads.iter().flatten() {
        let Code { code } = serde_json::from_str(payload).map_err(|_| corrupt("a refusal's row"))?;
        match refusals.iter_mut().find(|r| r.code == code) {
            Some(slot) => slot.count += 1,
            None => refusals.push(RefusalCount { code, count: 1 }),
        }
    }
    refusals.sort_by_key(|r| (Reverse(r.count), wire_name(&r.code)));
    Ok(refusals)
}

/// Which judge answered the questions that ended since `since`, as `admin status` counts them.
async fn judges(db: &Db, since: DateTime<Utc>) -> Result<JudgeCounts, ApiError> {
    let modes = status::modes_since(db, since).await.map_err(failed)?;
    Ok(JudgeCounts {
        jev: modes.jev,
        chat: modes.chat,
        rules: modes.rules,
        none: modes.none,
    })
}

/// How long the questions that finished in the window took, from asked to delivered. A question
/// held for a sign-off is left out: its duration runs to the approver's decision, up to a day, and
/// says how long the approver took, not the core.
async fn finished(db: &Db, from: &str) -> Result<Durations, ApiError> {
    let stored = sqlx::query_scalar!(
        r#"SELECT q.duration_ms AS "duration_ms!" FROM questions q
           WHERE q.finished_at >= ? AND q.duration_ms IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM audit_events a
                             WHERE a.question_id = q.id AND a.type = 'pending_signoff')
           ORDER BY q.duration_ms"#,
        from
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?;
    let sorted = stored
        .into_iter()
        .map(|ms| u64::try_from(ms).map_err(|_| corrupt("a question's duration")))
        .collect::<Result<Vec<u64>, _>>()?;
    Ok(durations(&sorted))
}

/// The sign-offs of the window's questions. One still pending past its expiry is counted as
/// expired: nobody decided within the day, and it can no longer be decided.
async fn signoffs(db: &Db, from: &str, now: DateTime<Utc>) -> Result<SignoffCounts, ApiError> {
    let rows = sqlx::query!(
        "SELECT ap.status, ap.expires_at FROM approvals ap JOIN questions q ON q.id = ap.question_id
         WHERE q.created_at >= ?",
        from
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?;
    let mut counts = SignoffCounts::default();
    for row in rows {
        match row.status.as_str() {
            "pending" if time(&row.expires_at, "a sign-off's expiry")? > now => counts.pending += 1,
            "pending" | "expired" => counts.expired += 1,
            "approved" => counts.approved += 1,
            "rejected" => counts.rejected += 1,
            _ => return Err(corrupt("a sign-off's status")),
        }
    }
    Ok(counts)
}

/// Today's spend (UTC), everyone's and each person's, against the budgets the core runs with.
async fn spend(db: &Db, config: &Config, now: DateTime<Utc>) -> Result<SpendToday, ApiError> {
    let day = budget::day(now);
    let rows = sqlx::query!(
        r#"SELECT s.user_id, s.spent_usd, u.email AS "email?", u.display_name AS "display_name?"
           FROM spend s LEFT JOIN users u ON u.id = s.user_id
           WHERE s.day = ? ORDER BY s.spent_usd DESC, s.user_id"#,
        day
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?;
    let mut everyone = 0.0;
    let mut people = Vec::new();
    for row in rows {
        if row.user_id == EVERYONE {
            everyone = row.spent_usd;
        } else if row.spent_usd > 0.0 {
            people.push(Spender {
                user_id: UserId::parse(&row.user_id).map_err(|_| corrupt("a spender's id"))?,
                email: row
                    .email
                    .as_deref()
                    .map(Email::parse)
                    .transpose()
                    .map_err(|_| corrupt("a spender's e-mail address"))?,
                display_name: row.display_name,
                spent_usd: usd(row.spent_usd)?,
            });
        }
    }
    let budgets = &config.models.budgets;
    Ok(SpendToday {
        day: now.date_naive(),
        everyone_usd: usd(everyone)?,
        daily_budget_usd: Some(budgets.global_daily_usd),
        per_user_daily_usd: Some(budgets.per_user_daily_usd),
        people,
    })
}

/// The alerts that hold at `now`, as `admin status` says them, against the configuration the core
/// loaded: its daily budget and the projects its `projects.yaml` lists.
async fn alerts_now(db: &Db, config: &Config, now: DateTime<Utc>) -> Result<Vec<AdminAlert>, ApiError> {
    let daily = config.models.budgets.global_daily_usd.get();
    let projects: Vec<ProjectId> = config.projects.projects.keys().cloned().collect();
    let found = alerts::check(db, Some(daily), Some(&projects), now)
        .await
        .map_err(failed)?;
    let none = AdminAlert {
        kind: AlertKind::Spend,
        project: None,
        since: None,
        judge_mode: None,
        spent_usd: None,
        budget_usd: None,
    };
    found
        .into_iter()
        .map(|alert| {
            Ok(match alert {
                Alert::Spend { spent, limit, .. } => AdminAlert {
                    spent_usd: Some(usd(spent)?),
                    budget_usd: Some(usd(limit)?),
                    ..none.clone()
                },
                Alert::JudgeDegraded { mode, since, .. } => AdminAlert {
                    kind: AlertKind::JudgeDegraded,
                    since: Some(since),
                    judge_mode: Some(match mode {
                        Degraded::Chat => JudgeMode::Chat,
                        Degraded::Rules => JudgeMode::Rules,
                    }),
                    ..none.clone()
                },
                // a probe that never registered has no last registration: `since` stays `null`
                Alert::ProbeSilent {
                    project: name,
                    last_seen,
                    ..
                } => AdminAlert {
                    kind: AlertKind::ProbeSilent,
                    project: Some(project(&name)?),
                    since: last_seen,
                    ..none.clone()
                },
            })
        })
        .collect()
}

/// The settings the core runs with, as it read them at its start: shown, never changed here.
fn settings(config: &Config) -> Settings {
    let roles = &config.models.roles;
    let judge = |role: ModelRole, judge: &JudgeRole| ModelRoleLine {
        role,
        backend: judge.backend,
        kind: Some(judge.kind),
        model: judge.model.clone(),
    };
    let chat = |role: ModelRole, chat: &ChatRole| ModelRoleLine {
        role,
        backend: chat.backend,
        kind: None,
        model: chat.model.clone(),
    };
    let mut models = vec![judge(ModelRole::Judge, &roles.judge)];
    if let Some(fallback) = &roles.judge_fallback {
        models.push(judge(ModelRole::JudgeFallback, fallback));
    }
    models.push(chat(ModelRole::Planner, &roles.planner));
    models.push(chat(ModelRole::Composer, &roles.composer));
    let budgets = &config.models.budgets;
    Settings {
        models: Some(models),
        budgets: Some(BudgetSettings {
            per_question_usd: budgets.per_question_usd,
            per_user_daily_usd: budgets.per_user_daily_usd,
            global_daily_usd: budgets.global_daily_usd,
        }),
        checks: config.checks.as_ref().map(|checks| {
            checks
                .0
                .iter()
                .map(|check| CheckLine {
                    id: check.id.clone(),
                    stage: check.stage,
                    block_at: check.thresholds.block_at,
                    review_at: check.thresholds.review_at,
                    pass_at: check.thresholds.pass_at,
                    clarify_below: check.thresholds.clarify_below,
                })
                .collect()
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// The probes

/// A probe's row as registration wrote it.
struct ProbeRow {
    project: String,
    access: String,
    access_limited_by: Option<String>,
    status: String,
    schema: String,
    manifest_json: String,
    tools_json: String,
    quarantine_reason: Option<String>,
    registered_at: String,
    last_seen_at: String,
}

async fn probe_rows(db: &Db) -> Result<Vec<ProbeRow>, ApiError> {
    sqlx::query_as!(
        ProbeRow,
        "SELECT project, access, access_limited_by, status, schema, manifest_json, tools_json,
                quarantine_reason, registered_at, last_seen_at
         FROM probes ORDER BY project"
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)
}

fn manifest(row: &ProbeRow) -> Result<ProbeManifest, ApiError> {
    serde_json::from_str(&row.manifest_json).map_err(|_| corrupt("a probe's manifest"))
}

/// The probe as the panel shows it, its status at `now`: a healthy probe silent for three minutes
/// shows as stale.
fn probe(row: &ProbeRow, now: DateTime<Utc>) -> Result<AdminProbe, ApiError> {
    let status = ProbeStatus::parse(&row.status).ok_or_else(|| corrupt("a probe's status"))?;
    let last_seen_at = time(&row.last_seen_at, "a probe's last registration")?;
    let status = match status.at(&row.last_seen_at, now) {
        ProbeStatus::Healthy => ApiProbeStatus::Healthy,
        ProbeStatus::Stale => ApiProbeStatus::Stale,
        ProbeStatus::Quarantined => ApiProbeStatus::Quarantined,
    };
    let manifest = manifest(row)?;
    let mut modules: Vec<ModuleName> = Vec::new();
    for module in manifest.modules.keys().chain(manifest.module_labels.keys()) {
        if !modules.contains(module) {
            modules.push(module.clone());
        }
    }
    Ok(AdminProbe {
        status,
        access: stored::<AccessMode>(&row.access, "a probe's access mode")?,
        access_limited_by: row
            .access_limited_by
            .as_deref()
            .map(|by| stored::<AccessLimit>(by, "what limited a probe"))
            .transpose()?,
        schema: stored::<SchemaId>(&row.schema, "a probe's schema")?,
        registered_at: time(&row.registered_at, "a probe's registration")?,
        last_seen_at,
        quarantine_reason: row.quarantine_reason.clone(),
        modules,
        tools: serde_json::from_str::<Vec<ToolName>>(&row.tools_json)
            .map_err(|_| corrupt("a probe's tools"))?,
    })
}

/// How many questions each person or project asked since `RECENT` ago, and when their last one
/// was, whenever that was.
#[derive(Debug, Default)]
struct Asked {
    recent: u64,
    last: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------------------------
// People

fn token_state(revoked: bool, expires_at: DateTime<Utc>, now: DateTime<Utc>) -> TokenState {
    match revoked {
        true => TokenState::Revoked,
        false if now >= expires_at => TokenState::Expired,
        false => TokenState::Active,
    }
}

/// What `GET /v1/admin/people` shows at `now`: everyone, disabled people too, by e-mail address,
/// what each grant lets them ask as `admin access list` reads it, and their tokens without a
/// secret.
pub(crate) async fn read_people(
    db: &Db,
    config: &Config,
    now: DateTime<Utc>,
) -> Result<AdminPeople, ApiError> {
    let mut registered: HashMap<String, ProbeManifest> = HashMap::new();
    for row in probe_rows(db).await? {
        registered.insert(row.project.clone(), manifest(&row)?);
    }

    let mut grants: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    for row in sqlx::query!("SELECT user_id, project, approver FROM user_projects ORDER BY project")
        .fetch_all(db.pool())
        .await
        .map_err(failed)?
    {
        grants
            .entry(row.user_id)
            .or_default()
            .push((row.project, row.approver != 0));
    }

    let mut tokens: HashMap<String, Vec<AdminToken>> = HashMap::new();
    for row in sqlx::query!(
        "SELECT public_id, user_id, label, created_at, expires_at, revoked_at, last_used_at
         FROM tokens ORDER BY created_at, public_id"
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?
    {
        let expires_at = time(&row.expires_at, "a token's expiry")?;
        let revoked_at = maybe_time(row.revoked_at.as_deref(), "a token's revocation")?;
        tokens.entry(row.user_id).or_default().push(AdminToken {
            public_id: TokenPublicId::parse(&row.public_id).map_err(|_| corrupt("a token's public id"))?,
            label: row.label,
            state: token_state(revoked_at.is_some(), expires_at, now),
            created_at: time(&row.created_at, "a token's creation")?,
            expires_at,
            revoked_at,
            last_used_at: maybe_time(row.last_used_at.as_deref(), "a token's last use")?,
        });
    }

    let recent = db::time(now - RECENT);
    let mut asked: HashMap<String, Asked> = HashMap::new();
    for row in sqlx::query!(
        r#"SELECT user_id AS "user_id!", SUM(created_at >= ?) AS "recent!: i64",
                  MAX(created_at) AS "last!: String"
           FROM questions GROUP BY user_id"#,
        recent
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?
    {
        let last = time(&row.last, "a question's time")?;
        asked.insert(
            row.user_id,
            Asked {
                recent: count(row.recent),
                last: Some(last),
            },
        );
    }

    let day = budget::day(now);
    let mut spent: HashMap<String, f64> = HashMap::new();
    for row in sqlx::query!("SELECT user_id, spent_usd FROM spend WHERE day = ?", day)
        .fetch_all(db.pool())
        .await
        .map_err(failed)?
    {
        spent.insert(row.user_id, row.spent_usd);
    }

    let mut people = Vec::new();
    for row in sqlx::query!(
        "SELECT id, email, display_name, role, disabled_at, created_at FROM users ORDER BY email"
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?
    {
        let role = parse_role(&row.role).ok_or_else(|| corrupt("a person's role"))?;
        let mut shown = Vec::new();
        for (name, approver) in grants.remove(&row.id).unwrap_or_default() {
            let classes = admin::grant_classes(
                &config.projects,
                &config.policies,
                role,
                &name,
                registered.get(&name),
            )
            .map_err(|_| corrupt("a grant's project"))?;
            shown.push(AdminGrant {
                project: project(&name)?,
                approver,
                classes: match classes {
                    Classes::Allowed(classes) => Some(
                        classes
                            .into_iter()
                            .map(|(class, signoff)| GrantClass { class, signoff })
                            .collect(),
                    ),
                    Classes::NoManifest => None,
                },
            });
        }
        let asked = asked.remove(&row.id).unwrap_or_default();
        people.push(AdminPerson {
            id: UserId::parse(&row.id).map_err(|_| corrupt("a person's id"))?,
            email: Email::parse(&row.email).map_err(|_| corrupt("a person's e-mail address"))?,
            display_name: row.display_name,
            role,
            created_at: time(&row.created_at, "a person's creation")?,
            disabled_at: maybe_time(row.disabled_at.as_deref(), "a person's disabling")?,
            grants: shown,
            tokens: tokens.remove(&row.id).unwrap_or_default(),
            questions_30d: asked.recent,
            last_asked_at: asked.last,
            spent_today_usd: usd(spent.get(&row.id).copied().unwrap_or(0.0))?,
        });
    }

    Ok(AdminPeople {
        people,
        projects: config.projects.projects.keys().cloned().collect(),
        // the core read policies.yaml and projects.yaml at its start, or it wouldn't run
        classes_known: true,
    })
}

// ---------------------------------------------------------------------------------------------
// Repositories

/// What `GET /v1/admin/repositories` shows at `now`: every project `projects.yaml` lists or a probe
/// registered, by project, with its probe and the people granted it.
pub(crate) async fn read_repositories(
    db: &Db,
    config: &Config,
    now: DateTime<Utc>,
) -> Result<AdminRepositories, ApiError> {
    let mut probes: HashMap<String, ProbeRow> = HashMap::new();
    for row in probe_rows(db).await? {
        probes.insert(row.project.clone(), row);
    }

    let mut people: HashMap<String, Vec<RepositoryPerson>> = HashMap::new();
    for row in sqlx::query!(
        "SELECT up.project, up.approver, u.email, u.display_name, u.role, u.disabled_at
         FROM user_projects up JOIN users u ON u.id = up.user_id ORDER BY u.email"
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?
    {
        people.entry(row.project).or_default().push(RepositoryPerson {
            email: Email::parse(&row.email).map_err(|_| corrupt("a person's e-mail address"))?,
            display_name: row.display_name,
            role: parse_role(&row.role).ok_or_else(|| corrupt("a person's role"))?,
            approver: row.approver != 0,
            disabled_at: maybe_time(row.disabled_at.as_deref(), "a person's disabling")?,
        });
    }

    let recent = db::time(now - RECENT);
    let mut asked: HashMap<String, Asked> = HashMap::new();
    for row in sqlx::query!(
        r#"SELECT project AS "project!", SUM(created_at >= ?) AS "recent!: i64",
                  MAX(created_at) AS "last!: String"
           FROM questions WHERE project IS NOT NULL GROUP BY project"#,
        recent
    )
    .fetch_all(db.pool())
    .await
    .map_err(failed)?
    {
        let last = time(&row.last, "a question's time")?;
        asked.insert(
            row.project,
            Asked {
                recent: count(row.recent),
                last: Some(last),
            },
        );
    }

    let mut ids: BTreeSet<String> = config
        .projects
        .projects
        .keys()
        .map(|id| id.as_str().to_owned())
        .collect();
    ids.extend(probes.keys().cloned());
    let mut repositories = Vec::with_capacity(ids.len());
    for id in ids {
        let listed = ProjectId::parse(&id)
            .ok()
            .and_then(|parsed| config.projects.projects.get(&parsed));
        let asked = asked.remove(&id).unwrap_or_default();
        repositories.push(AdminRepository {
            project: project(&id)?,
            display_name: listed.map(|p| p.display_name.clone()),
            access_max: listed.map(|p| p.access_max),
            probe: probes.get(&id).map(|row| probe(row, now)).transpose()?,
            people: people.remove(&id).unwrap_or_default(),
            questions_30d: asked.recent,
            last_asked_at: asked.last,
        });
    }
    Ok(AdminRepositories { repositories })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mercury_schema::Role;
    use mercury_schema::events::{ComposerMode, Done, Event, PendingSignoff, Refused};
    use mercury_schema::ids::ApprovalId;
    use mercury_schema::{QuestionId, api::Refusal, api::SignoffReason};

    use super::*;
    use crate::db::testing;

    const NOW: &str = "2026-10-08T12:34:00.000Z";

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    fn config() -> Config {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
        crate::config::load(&examples).unwrap().0
    }

    #[test]
    fn each_window_is_cut_into_its_buckets_ending_with_the_current_one() {
        let now = at(NOW);
        assert_eq!(
            span(OverviewWindow::Day, now),
            (at("2026-10-07T13:00:00.000Z"), Bucket::Hour, 24)
        );
        assert_eq!(
            span(OverviewWindow::Week, now),
            (at("2026-10-02T00:00:00.000Z"), Bucket::Day, 7)
        );
        assert_eq!(
            span(OverviewWindow::Month, now),
            (at("2026-09-09T00:00:00.000Z"), Bucket::Day, 30)
        );
    }

    #[test]
    fn the_percentiles_are_by_nearest_rank() {
        assert_eq!(durations(&[]), Durations::default());
        let one = Durations {
            p50: Some(7),
            p95: Some(7),
            max: Some(7),
        };
        assert_eq!(durations(&[7]), one);
        let ten: Vec<u64> = (1..=10).map(|n| n * 100).collect();
        assert_eq!(
            durations(&ten),
            Durations {
                p50: Some(500),
                p95: Some(1_000),
                max: Some(1_000)
            }
        );
        let twenty: Vec<u64> = (1..=20).collect();
        assert_eq!(durations(&twenty).p95, Some(19));
    }

    #[test]
    fn amounts_are_never_negative_and_rounding_reads_as_zero() {
        assert_eq!(usd(-1e-12).unwrap().get(), 0.0);
        assert_eq!(usd(0.25).unwrap().get(), 0.25);
        assert!(usd(-0.01).is_err());
        assert!(usd(f64::NAN).is_err());
    }

    /// A person with a token, by e-mail address; their id and the token's public id.
    async fn person(db: &Db, email: &str, role: Role) -> (String, String) {
        let id = admin::add_user(db, email, "Pat Example", role, at(NOW))
            .await
            .unwrap();
        let created = admin::create_token(db, email, "laptop", TimeDelta::days(90), at(NOW))
            .await
            .unwrap();
        (id, created.token.public_id().to_owned())
    }

    struct Seed<'a> {
        n: u32,
        channel: &'a str,
        project: Option<&'a str>,
        class: Option<&'a str>,
        status: &'a str,
        judge: Option<&'a str>,
        cost: f64,
        duration_ms: Option<i64>,
        at: &'a str,
    }

    fn qid(n: u32) -> QuestionId {
        QuestionId::parse(&format!("q_01J9Z3QK7M8N9P0R1S2T3V{n:04}")).unwrap()
    }

    async fn ask(db: &Db, who: &(String, String), q: Seed<'_>) {
        let finished = q.duration_ms.map(|_| q.at);
        sqlx::query(
            "INSERT INTO questions (id, user_id, token_public_id, channel, project, class, status, text_redacted,
                                    judge_mode, cost_usd, duration_ms, created_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'what was merged?', ?, ?, ?, ?, ?)",
        )
        .bind(qid(q.n).as_str())
        .bind(&who.0)
        .bind(&who.1)
        .bind(q.channel)
        .bind(q.project)
        .bind(q.class)
        .bind(q.status)
        .bind(q.judge)
        .bind(q.cost)
        .bind(q.duration_ms)
        .bind(q.at)
        .bind(finished)
        .execute(db.pool())
        .await
        .unwrap();
    }

    async fn refuse(db: &Db, n: u32, code: RefusalCode, when: &str) {
        let refusal = Refusal {
            code,
            message: "You can't ask that.".to_owned(),
            hint: "Ask about a module.".to_owned(),
        };
        let event = Event::Refused(Refused::new(refusal, Usd::default(), Some(JudgeMode::Jev)));
        let _recorded = audit::record(db, &qid(n), None, event, at(when)).await.unwrap();
    }

    async fn hold(db: &Db, n: u32, status: &str, expires: &str, when: &str) {
        let approval = format!("ap_01JB2M3N4P5Q6R7S8T9V0W{n:04}");
        let event = Event::PendingSignoff(PendingSignoff {
            approval_id: ApprovalId::parse(&approval).unwrap(),
            reason: SignoffReason::Class,
        });
        let _recorded = audit::record(db, &qid(n), None, event, at(when)).await.unwrap();
        sqlx::query(
            "INSERT INTO approvals (id, question_id, reason_json, draft_answer, status, created_at, expires_at)
             VALUES (?, ?, '{}', '{}', ?, ?, ?)",
        )
        .bind(&approval)
        .bind(qid(n).as_str())
        .bind(status)
        .bind(when)
        .bind(expires)
        .execute(db.pool())
        .await
        .unwrap();
    }

    async fn done(db: &Db, n: u32, mode: JudgeMode, when: &str) {
        let event = Event::Done(Done {
            duration_ms: 1_000,
            cost_usd: Usd::new(0.01).unwrap(),
            judge_mode: mode,
            composer: ComposerMode::Model,
            audit_id: 1,
        });
        let _recorded = audit::record(db, &qid(n), None, event, at(when)).await.unwrap();
    }

    /// A week of questions: each outcome, each channel but `mcp`, a question no project took, a
    /// refusal, a held one, a degraded one, and one before the window.
    async fn seeded() -> (tempfile::TempDir, Db) {
        let (dir, db) = testing::db().await;
        let pat = person(&db, "pat@example.com", Role::Pm).await;
        let lee = person(&db, "lee@example.com", Role::TechLead).await;
        let base = Seed {
            n: 0,
            channel: "cli",
            project: Some("delta"),
            class: Some("progress"),
            status: "delivered",
            judge: Some("jev"),
            cost: 0.01,
            duration_ms: Some(1_000),
            at: "2026-10-08T09:00:00.000Z",
        };
        // answered by Jev, on the cli and the web
        ask(&db, &pat, Seed { n: 1, ..base }).await;
        ask(
            &db,
            &pat,
            Seed {
                n: 2,
                channel: "web",
                duration_ms: Some(3_000),
                at: "2026-10-07T10:00:00.000Z",
                ..base
            },
        )
        .await;
        // degraded: delivered by the rules
        ask(
            &db,
            &lee,
            Seed {
                n: 3,
                judge: Some("rules"),
                class: Some("tested"),
                duration_ms: Some(2_000),
                at: "2026-10-06T08:00:00.000Z",
                ..base
            },
        )
        .await;
        // refused before any project was taken
        ask(
            &db,
            &lee,
            Seed {
                n: 4,
                channel: "web",
                project: None,
                class: Some("other"),
                status: "refused",
                judge: Some("jev"),
                cost: 0.002,
                duration_ms: None,
                at: "2026-10-05T08:00:00.000Z",
            },
        )
        .await;
        refuse(&db, 4, RefusalCode::OutOfScope, "2026-10-05T08:00:01.000Z").await;
        // held for a sign-off and still waiting, and one held and rejected by the approver
        ask(
            &db,
            &pat,
            Seed {
                n: 5,
                status: "pending_signoff",
                judge: None,
                duration_ms: None,
                at: "2026-10-08T11:00:00.000Z",
                ..base
            },
        )
        .await;
        hold(
            &db,
            5,
            "pending",
            "2026-10-09T11:00:00.000Z",
            "2026-10-08T11:00:01.000Z",
        )
        .await;
        ask(
            &db,
            &pat,
            Seed {
                n: 6,
                status: "refused",
                duration_ms: None,
                at: "2026-10-04T08:00:00.000Z",
                ..base
            },
        )
        .await;
        hold(
            &db,
            6,
            "rejected",
            "2026-10-05T08:00:00.000Z",
            "2026-10-04T08:00:01.000Z",
        )
        .await;
        refuse(
            &db,
            6,
            RefusalCode::RejectedByApprover,
            "2026-10-04T09:00:00.000Z",
        )
        .await;
        // still running
        ask(
            &db,
            &lee,
            Seed {
                n: 7,
                status: "running",
                judge: None,
                class: None,
                duration_ms: None,
                at: "2026-10-08T12:30:00.000Z",
                ..base
            },
        )
        .await;
        // before the week: counted nowhere
        ask(
            &db,
            &pat,
            Seed {
                n: 8,
                at: "2026-10-01T23:59:59.000Z",
                ..base
            },
        )
        .await;
        done(&db, 1, JudgeMode::Jev, "2026-10-08T09:00:06.000Z").await;
        done(&db, 3, JudgeMode::Rules, "2026-10-06T08:00:02.000Z").await;
        (dir, db)
    }

    #[tokio::test]
    async fn the_overview_counts_the_window_by_outcome_channel_project_class_and_refusal() {
        let (_dir, db) = seeded().await;
        let config = config();
        let week = read_overview(&db, &config, OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert_eq!(
            (week.since, week.until),
            (at("2026-10-02T00:00:00.000Z"), at(NOW))
        );
        assert_eq!(
            week.totals,
            OutcomeCounts {
                answered: 2,
                refused: 1,
                degraded: 1,
                held: 2,
                other: 1
            },
            "the rejected one is held, the one before the week isn't counted"
        );
        // seven days, oldest first, the empty ones included, the last one today
        assert_eq!(week.series.len(), 7);
        assert_eq!(week.series[0].start, at("2026-10-02T00:00:00.000Z"));
        assert_eq!(week.series[6].start, at("2026-10-08T00:00:00.000Z"));
        assert_eq!(week.series[0].counts, OutcomeCounts::default());
        let today = week.series[6].counts;
        assert_eq!((today.answered, today.held, today.other), (1, 1, 1));
        let summed: u64 = week.series.iter().map(|p| total(&p.counts)).sum();
        assert_eq!(summed, total(&week.totals));
        // every channel, `mcp` with nothing
        let channels: Vec<(Channel, u64)> = week
            .channels
            .iter()
            .map(|c| (c.channel, total(&c.counts)))
            .collect();
        assert_eq!(
            channels,
            [(Channel::Cli, 5), (Channel::Web, 2), (Channel::Mcp, 0)]
        );
        // delta first, the question no project took after it
        assert_eq!(week.projects.len(), 2);
        assert_eq!(
            week.projects[0].project.as_ref().map(ProjectId::as_str),
            Some("delta")
        );
        assert_eq!(total(&week.projects[0].counts), 6);
        assert!((week.projects[0].cost_usd.get() - 0.06).abs() < 1e-9);
        assert_eq!(week.projects[1].project, None);
        assert_eq!(week.projects[1].counts.refused, 1);
        assert_eq!(
            week.classes,
            [
                ClassCount {
                    class: QuestionClass::Progress,
                    count: 4
                },
                ClassCount {
                    class: QuestionClass::Other,
                    count: 1
                },
                ClassCount {
                    class: QuestionClass::Tested,
                    count: 1
                }
            ]
        );
        assert_eq!(
            week.refusals,
            [
                RefusalCount {
                    code: RefusalCode::OutOfScope,
                    count: 1
                },
                RefusalCount {
                    code: RefusalCode::RejectedByApprover,
                    count: 1
                }
            ]
        );
        // the endings of the week: two done, two refused
        assert_eq!(
            week.judge,
            JudgeCounts {
                jev: 3,
                chat: 0,
                rules: 1,
                none: 0
            }
        );
        assert_eq!(
            week.duration_ms,
            Durations {
                p50: Some(2_000),
                p95: Some(3_000),
                max: Some(3_000)
            }
        );
        assert!((week.cost_usd.get() - 0.062).abs() < 1e-9, "{:?}", week.cost_usd);
        assert_eq!(week.askers, 2);
        assert_eq!(
            week.signoffs,
            SignoffCounts {
                pending: 1,
                approved: 0,
                rejected: 1,
                expired: 0
            }
        );
        // the settings the core runs with
        let models = week.settings.models.as_ref().unwrap();
        assert_eq!(models.len(), 4);
        assert_eq!(models[0].role, ModelRole::Judge);
        assert_eq!(models[0].kind, Some(mercury_schema::config::JudgeKind::Jev));
        assert_eq!(models[2].kind, None, "the planner is no judge");
        assert_eq!(week.settings.budgets.unwrap().global_daily_usd.get(), 2.0);
        let checks = week.settings.checks.as_ref().unwrap();
        assert!(
            checks
                .iter()
                .any(|c| c.id.as_str() == "asks_for_code" && c.block_at.is_some())
        );
    }

    #[tokio::test]
    async fn a_held_answer_is_left_out_of_the_durations() {
        let (_dir, db) = seeded().await;
        let kim = person(&db, "kim@example.com", Role::Pm).await;
        // approved almost a day after it was asked: the wait is the approver's
        ask(
            &db,
            &kim,
            Seed {
                n: 9,
                channel: "web",
                project: Some("delta"),
                class: Some("progress"),
                status: "delivered",
                judge: Some("jev"),
                cost: 0.01,
                duration_ms: Some(86_000_000),
                at: "2026-10-07T09:00:00.000Z",
            },
        )
        .await;
        hold(
            &db,
            9,
            "approved",
            "2026-10-08T09:00:00.000Z",
            "2026-10-07T09:00:01.000Z",
        )
        .await;
        let week = read_overview(&db, &config(), OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert_eq!(week.totals.held, 3, "it is counted, as held");
        assert_eq!(
            week.duration_ms,
            Durations {
                p50: Some(2_000),
                p95: Some(3_000),
                max: Some(3_000)
            },
            "the durations of the ones that weren't held"
        );
    }

    #[tokio::test]
    async fn the_day_and_the_month_have_their_own_buckets() {
        let (_dir, db) = seeded().await;
        let config = config();
        let day = read_overview(&db, &config, OverviewWindow::Day, at(NOW))
            .await
            .unwrap();
        assert_eq!(day.bucket, Bucket::Hour);
        assert_eq!(day.series.len(), 24);
        assert_eq!(day.series[23].start, at("2026-10-08T12:00:00.000Z"));
        assert_eq!(day.series[23].counts.other, 1, "the one asked at 12:30");
        assert_eq!(day.series[22].counts.held, 1, "the one asked at 11:00");
        assert_eq!(day.series[20].counts.answered, 1, "the one asked at 09:00");
        assert_eq!(total(&day.totals), 3);
        assert_eq!(day.askers, 2);
        assert_eq!(day.judge.jev, 1);

        let month = read_overview(&db, &config, OverviewWindow::Month, at(NOW))
            .await
            .unwrap();
        assert_eq!(month.bucket, Bucket::Day);
        assert_eq!(month.series.len(), 30);
        assert_eq!(total(&month.totals), 8, "the one of 1 October too");
    }

    #[tokio::test]
    async fn a_quiet_window_still_has_every_bucket_and_channel() {
        let (_dir, db) = testing::db().await;
        let week = read_overview(&db, &config(), OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert_eq!(week.series.len(), 7);
        assert_eq!(week.channels.len(), 3);
        assert!(week.projects.is_empty() && week.classes.is_empty() && week.refusals.is_empty());
        assert_eq!(week.duration_ms, Durations::default());
        assert_eq!(week.askers, 0);
        assert_eq!(week.spend.everyone_usd.get(), 0.0);
        assert!(week.alerts.is_empty());
    }

    #[tokio::test]
    async fn today_spend_and_the_alerts_are_those_admin_status_says() {
        let (_dir, db) = seeded().await;
        let (pat,): (String,) = sqlx::query_as("SELECT id FROM users WHERE email = 'pat@example.com'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        for (who, usd) in [(pat.as_str(), 0.4), ("*", 1.7)] {
            sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES ('2026-10-08', ?, ?)")
                .bind(who)
                .bind(usd)
                .execute(db.pool())
                .await
                .unwrap();
        }
        // yesterday's is another day's
        sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES ('2026-10-07', ?, 0.9)")
            .bind(&pat)
            .execute(db.pool())
            .await
            .unwrap();
        let week = read_overview(&db, &config(), OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert_eq!(week.spend.day.to_string(), "2026-10-08");
        assert_eq!(week.spend.everyone_usd.get(), 1.7);
        assert_eq!(week.spend.daily_budget_usd.map(Usd::get), Some(2.0));
        assert_eq!(week.spend.people.len(), 1);
        assert_eq!(
            week.spend.people[0].email.as_ref().map(Email::as_str),
            Some("pat@example.com")
        );
        let kinds: Vec<AlertKind> = week.alerts.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [AlertKind::Spend], "85% of the daily budget");
        assert_eq!(week.alerts[0].spent_usd.map(Usd::get), Some(1.7));
        assert_eq!(week.alerts[0].budget_usd.map(Usd::get), Some(2.0));
    }

    #[tokio::test]
    async fn a_listed_project_whose_probe_never_registered_is_a_silent_probe_without_a_since() {
        let (_dir, db) = testing::db().await;
        let config = config();
        // delta, which the examples list, has no probe; the audit began an hour ago
        done(&db, 1, JudgeMode::Jev, "2026-10-08T11:34:00.000Z").await;
        let week = read_overview(&db, &config, OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert_eq!(
            week.alerts,
            [AdminAlert {
                kind: AlertKind::ProbeSilent,
                project: Some(ProjectId::parse("delta").unwrap()),
                since: None,
                judge_mode: None,
                spent_usd: None,
                budget_usd: None,
            }]
        );

        // a fresh audit raises nothing
        let (_fresh_dir, fresh) = testing::db().await;
        done(&fresh, 1, JudgeMode::Jev, "2026-10-08T12:30:00.000Z").await;
        let week = read_overview(&fresh, &config, OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        assert!(week.alerts.is_empty(), "{:?}", week.alerts);
    }

    #[tokio::test]
    async fn a_daily_budget_of_zero_is_a_spend_alert_with_its_amounts() {
        let (_dir, db) = testing::db().await;
        let mut config = config();
        config.models.budgets.global_daily_usd = Usd::new(0.0).unwrap();
        let week = read_overview(&db, &config, OverviewWindow::Week, at(NOW))
            .await
            .unwrap();
        let alerts: Vec<(AlertKind, Option<f64>, Option<f64>)> = week
            .alerts
            .iter()
            .map(|a| (a.kind, a.spent_usd.map(Usd::get), a.budget_usd.map(Usd::get)))
            .collect();
        assert_eq!(
            alerts,
            [(AlertKind::Spend, Some(0.0), Some(0.0))],
            "nothing spent, and nothing can be asked"
        );
        assert_eq!(week.spend.daily_budget_usd.map(Usd::get), Some(0.0));
    }

    #[tokio::test]
    async fn a_stored_row_that_doesnt_read_fails_the_overview_instead_of_a_guess() {
        let (_dir, db) = seeded().await;
        sqlx::query("UPDATE questions SET channel = 'fax' WHERE id = ?")
            .bind(qid(1).as_str())
            .execute(db.pool())
            .await
            .unwrap();
        let failed = read_overview(&db, &config(), OverviewWindow::Week, at(NOW)).await;
        assert_eq!(failed.unwrap_err().code, ErrorCode::Internal);
    }
}
