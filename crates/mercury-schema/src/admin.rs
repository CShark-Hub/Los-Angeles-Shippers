//! The bodies of the read-only admin endpoints, `GET /v1/admin/*`, which the web client's admin
//! panel shows: what Mercury did over a window, who may ask about what, and each repository's
//! probe.
//!
//! Aggregates and metadata only: no token's secret, no question's text, no answer, no fact and no
//! draft. Nothing here changes anything; people, roles, grants, tokens and a probe's quarantine
//! change through the admin commands on the server, and the settings it shows (the access modes,
//! the model backends, the budgets, the thresholds) through the files there.
//!
//! As for every response, unknown fields are accepted and every field is present: `null` when
//! there is nothing to say.

use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::{ProbeStatus, RefusalCode, Usd};
use crate::config::{BackendName, CheckStage, JudgeKind};
use crate::enums::{AccessLimit, AccessMode, Channel, JudgeMode, QuestionClass, Role, ToolName};
use crate::facts::{Ratio, SchemaId};
use crate::ids::{CheckId, Email, ModelSlug, ModuleName, ProjectId, TokenPublicId, UserId};

/// The span `GET /v1/admin/overview` counts over, back from the request's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub enum OverviewWindow {
    /// The last 24 hours, in hours.
    #[serde(rename = "24h")]
    Day,
    /// The last 7 days, in days; the default.
    #[default]
    #[serde(rename = "7d")]
    Week,
    /// The last 30 days, in days.
    #[serde(rename = "30d")]
    Month,
}

/// How the overview's series cuts its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Hour,
    /// A UTC day.
    Day,
}

/// `GET /v1/admin/overview?window=…`: the questions asked in the window and what became of them,
/// today's spend, the alerts that hold, and the settings the core runs with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminOverview {
    pub window: OverviewWindow,
    /// The window's start, included.
    pub since: DateTime<Utc>,
    /// The request's time, the window's end.
    pub until: DateTime<Utc>,
    pub bucket: Bucket,
    /// Every question asked in the window, by outcome.
    pub totals: OutcomeCounts,
    /// The window in buckets, oldest first, the empty ones included; the first starts at or
    /// before `since`.
    pub series: Vec<SeriesPoint>,
    /// `cli`, `web` and `mcp`, each listed even with nothing asked.
    pub channels: Vec<ChannelCount>,
    /// The projects asked about, the most asked first.
    pub projects: Vec<ProjectCount>,
    /// The classes the questions were given, the most first.
    pub classes: Vec<ClassCount>,
    /// Why questions were refused, the most first.
    pub refusals: Vec<RefusalCount>,
    /// Which judge answered the questions that ended in the window, by the weakest any stage had.
    pub judge: JudgeCounts,
    /// How long the questions that finished in the window took, from asked to ended, leaving out
    /// those held for a sign-off, whose time includes the wait for the approver.
    pub duration_ms: Durations,
    /// What the window's questions cost.
    pub cost_usd: Usd,
    /// How many people asked in the window.
    pub askers: u64,
    /// The sign-offs of the window's questions.
    pub signoffs: SignoffCounts,
    /// Today's spend (UTC), whatever the window.
    pub spend: SpendToday,
    /// The alerts that hold now.
    pub alerts: Vec<AdminAlert>,
    pub settings: Settings,
}

/// Questions by what became of them; each question is in exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct OutcomeCounts {
    /// Delivered, judged by Jev.
    pub answered: u64,
    pub refused: u64,
    /// Delivered, judged by the chat fallback or the rules.
    pub degraded: u64,
    /// Went to a sign-off, whatever was decided.
    pub held: u64,
    /// A clarify, a failure, or still running.
    pub other: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct SeriesPoint {
    pub start: DateTime<Utc>,
    pub counts: OutcomeCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ChannelCount {
    pub channel: Channel,
    pub counts: OutcomeCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ProjectCount {
    /// `null` for the questions that never reached a project (refused or clarified first).
    #[schema(required = true)]
    pub project: Option<ProjectId>,
    pub counts: OutcomeCounts,
    pub cost_usd: Usd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ClassCount {
    pub class: QuestionClass,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct RefusalCount {
    pub code: RefusalCode,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct JudgeCounts {
    pub jev: u64,
    pub chat: u64,
    pub rules: u64,
    /// No judge answered: a question refused before any check ran.
    pub none: u64,
}

/// Each `null` when nothing finished in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Durations {
    #[schema(required = true)]
    pub p50: Option<u64>,
    #[schema(required = true)]
    pub p95: Option<u64>,
    #[schema(required = true)]
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct SignoffCounts {
    /// Waiting for an approver now.
    pub pending: u64,
    pub approved: u64,
    pub rejected: u64,
    /// Nobody decided within 24 hours.
    pub expired: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct SpendToday {
    /// The UTC day the spend is counted on.
    pub day: NaiveDate,
    pub everyone_usd: Usd,
    /// `global_daily_usd` from `models.yaml`; `null` when the file can't be read.
    #[schema(required = true)]
    pub daily_budget_usd: Option<Usd>,
    /// `per_user_daily_usd` from `models.yaml`; `null` when the file can't be read.
    #[schema(required = true)]
    pub per_user_daily_usd: Option<Usd>,
    /// The people who spent anything today, the most first.
    pub people: Vec<Spender>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Spender {
    pub user_id: UserId,
    /// `null` for a person no longer in the database.
    #[schema(required = true)]
    pub email: Option<Email>,
    #[schema(required = true)]
    pub display_name: Option<String>,
    pub spent_usd: Usd,
}

/// One of the three alerts `mercury-core admin status` says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminAlert {
    pub kind: AlertKind,
    /// The silent probe's project; `null` for the other kinds.
    #[schema(required = true)]
    pub project: Option<ProjectId>,
    /// Since when it holds: the first degraded ending, or the probe's last registration; `null`
    /// for the spend, and for a probe never seen.
    #[schema(required = true)]
    pub since: Option<DateTime<Utc>>,
    /// The judge the questions ended on; `null` but for `judge_degraded`.
    #[schema(required = true)]
    pub judge_mode: Option<JudgeMode>,
    /// Today's spend and the daily budget; `null` but for `spend`.
    #[schema(required = true)]
    pub spent_usd: Option<Usd>,
    #[schema(required = true)]
    pub budget_usd: Option<Usd>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// Everyone's spend today at 80% of the daily budget or more.
    Spend,
    /// The judge degraded (chat or rules) for more than 5 minutes.
    JudgeDegraded,
    /// A probe not quarantined and silent for more than 15 minutes, or a project of
    /// `projects.yaml` whose probe has never registered (`since` is `null`) once the audit's first
    /// row is 15 minutes old.
    ProbeSilent,
}

/// What the core runs with, from its files: shown, never changed here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Settings {
    /// Each model role's backend; `null` when `models.yaml` can't be read.
    #[schema(required = true)]
    pub models: Option<Vec<ModelRoleLine>>,
    /// `null` when `models.yaml` can't be read.
    #[schema(required = true)]
    pub budgets: Option<BudgetSettings>,
    /// The judge's checks and their thresholds; `null` when `checks.yaml` can't be read.
    #[schema(required = true)]
    pub checks: Option<Vec<CheckLine>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ModelRoleLine {
    pub role: ModelRole,
    pub backend: BackendName,
    /// `jev` or `chat` for the judge's roles; `null` for the planner and the composer.
    #[schema(required = true)]
    pub kind: Option<JudgeKind>,
    pub model: ModelSlug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Judge,
    JudgeFallback,
    Planner,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct BudgetSettings {
    pub per_question_usd: Usd,
    pub per_user_daily_usd: Usd,
    pub global_daily_usd: Usd,
}

/// A check of the judge's catalogue and the thresholds it is held to; a threshold the check
/// doesn't use is `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct CheckLine {
    pub id: CheckId,
    pub stage: CheckStage,
    #[schema(required = true)]
    pub block_at: Option<Ratio>,
    #[schema(required = true)]
    pub review_at: Option<Ratio>,
    #[schema(required = true)]
    pub pass_at: Option<Ratio>,
    #[schema(required = true)]
    pub clarify_below: Option<Ratio>,
}

/// `GET /v1/admin/people`: everyone in the database, disabled people too, with their grants,
/// what each grant lets them ask, and their tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminPeople {
    /// By e-mail address.
    pub people: Vec<AdminPerson>,
    /// The projects `projects.yaml` lists, which a grant may name; empty when it can't be read.
    pub projects: Vec<ProjectId>,
    /// False when `policies.yaml` or `projects.yaml` can't be read: every grant's `classes` is
    /// then `null`.
    pub classes_known: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminPerson {
    pub id: UserId,
    pub email: Email,
    pub display_name: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub disabled_at: Option<DateTime<Utc>>,
    /// By project.
    pub grants: Vec<AdminGrant>,
    /// Oldest first; never a secret.
    pub tokens: Vec<AdminToken>,
    /// The questions they asked in the last 30 days.
    pub questions_30d: u64,
    #[schema(required = true)]
    pub last_asked_at: Option<DateTime<Utc>>,
    /// Their spend today (UTC).
    pub spent_today_usd: Usd,
}

/// A project granted, or the pseudo-project `portfolio`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminGrant {
    pub project: ProjectId,
    pub approver: bool,
    /// What the grant lets its person ask: the classes of their role in `policies.yaml` and the
    /// project's manifest combined, as the core applies them. `null` when the project has no
    /// manifest yet (it can't be asked) or the files can't be read.
    #[schema(required = true)]
    pub classes: Option<Vec<GrantClass>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct GrantClass {
    pub class: QuestionClass,
    /// Its answers wait for an approver.
    pub signoff: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminToken {
    pub public_id: TokenPublicId,
    /// What the admin named it for, the device say.
    pub label: String,
    pub state: TokenState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[schema(required = true)]
    pub revoked_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenState {
    Active,
    Expired,
    Revoked,
}

/// `GET /v1/admin/repositories`: every project `projects.yaml` lists or a probe registered, with
/// its probe and the people who may ask about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminRepositories {
    /// By project.
    pub repositories: Vec<AdminRepository>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminRepository {
    pub project: ProjectId,
    /// From `projects.yaml`; `null` for a probe registered under a project the file doesn't list,
    /// or when it can't be read.
    #[schema(required = true)]
    pub display_name: Option<String>,
    /// The most `projects.yaml` lets its probe read; `null` as for `display_name`.
    #[schema(required = true)]
    pub access_max: Option<AccessMode>,
    /// `null` until the project's probe has registered.
    #[schema(required = true)]
    pub probe: Option<AdminProbe>,
    /// The people granted the project, by e-mail address.
    pub people: Vec<RepositoryPerson>,
    /// The questions asked about it in the last 30 days.
    pub questions_30d: u64,
    #[schema(required = true)]
    pub last_asked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AdminProbe {
    /// At the time of the request: a healthy probe silent for three minutes shows as stale.
    pub status: ProbeStatus,
    /// The mode it reads with.
    pub access: AccessMode,
    /// What kept it below `access_max`; `null` when nothing did.
    #[schema(required = true)]
    pub access_limited_by: Option<AccessLimit>,
    pub schema: SchemaId,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// The reason the admin gave, while it is quarantined.
    #[schema(required = true)]
    pub quarantine_reason: Option<String>,
    /// From its `describe`.
    pub modules: Vec<ModuleName>,
    pub tools: Vec<ToolName>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct RepositoryPerson {
    pub email: Email,
    pub display_name: String,
    pub role: Role,
    pub approver: bool,
    #[schema(required = true)]
    pub disabled_at: Option<DateTime<Utc>>,
}
