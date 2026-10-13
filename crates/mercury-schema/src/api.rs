//! The bodies of the core's HTTP API, from the CLI, the web client and the probes.
//!
//! Requests reject unknown fields, so a misspelt field fails instead of being ignored. Responses
//! accept them, so a client keeps working when a field is added (additive changes stay in `/v1`).
//! Every response field is present: `null` when there is nothing to say, never left out. The few
//! that are left out say so: a held draft and its flags on the caller's own question, which the
//! asker never reads before the decision, and the `follows` of an `accepted` event.

use std::borrow::Cow;
use std::fmt;

use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

use crate::enums::{AccessLimit, AccessMode, JudgeMode, QuestionClass, Role, ToolName};
use crate::facts::{Fact, Ratio, SchemaId};
use crate::ids::{
    ApprovalId, CheckId, Email, EventType, ModuleName, ProjectId, QuestionId, Sha256Hex, TokenPublicId,
    UserId,
};
use crate::manifest::ProbeManifest;

/// An amount in US dollars: finite and never negative.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Usd(f64);

impl Usd {
    pub fn new(value: f64) -> Option<Self> {
        (value.is_finite() && value >= 0.0).then_some(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for Usd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for Usd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = f64::deserialize(deserializer)?;
        Usd::new(raw).ok_or_else(|| serde::de::Error::custom("an amount in dollars is never negative"))
    }
}

impl JsonSchema for Usd {
    fn schema_name() -> Cow<'static, str> {
        "Usd".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "number", "minimum": 0, "description": "US dollars." })
    }
}

/// A personal token as the person pasted it. `Debug` never prints it, and nothing here logs it;
/// the core looks it up and checks it like any Bearer token.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSecret(String);

impl TokenSecret {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenSecret(<redacted>)")
    }
}

impl Serialize for TokenSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TokenSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl JsonSchema for TokenSecret {
    fn schema_name() -> Cow<'static, str> {
        "TokenSecret".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "password",
            "writeOnly": true,
            "description": "A personal token, mcy_<public_id>_<secret>."
        })
    }
}

/// `GET /v1/health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ServiceHealth {
    pub status: HealthStatus,
    /// The product version, `0.0.0` until a release.
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
}

/// `POST /v1/session`: the web client's sign-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    pub token: TokenSecret,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Me {
    pub user: MeUser,
    pub projects: Vec<Grant>,
    pub token: MeToken,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MeUser {
    pub id: UserId,
    pub email: Email,
    pub display_name: String,
    pub role: Role,
}

/// A project the person was granted, or the pseudo-project `portfolio`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Grant {
    pub project: ProjectId,
    pub approver: bool,
}

/// The token the request came with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MeToken {
    pub public_id: TokenPublicId,
    pub expires_at: DateTime<Utc>,
}

/// `GET /v1/projects`: the projects the person may ask about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ProjectList {
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ProjectSummary {
    pub project: ProjectId,
    pub display_name: String,
    /// From the probe's `describe`.
    pub modules: Vec<ModuleName>,
    /// The classes the effective policy allows this person here.
    pub classes: Vec<QuestionClass>,
    /// `null` until the project's probe has registered.
    #[schema(required = true)]
    pub probe: Option<ProbeState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ProbeState {
    pub status: ProbeStatus,
    /// When the probe's data was last fresh.
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Healthy,
    /// No heartbeat for 3 minutes: answers may be out of date.
    Stale,
    /// Never called until an admin releases it.
    Quarantined,
}

/// `POST /v1/questions`, with the `Idempotency-Key` header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewQuestion {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// The option picked after a `clarify`; the text stays the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarify: Option<ClarifyAnswer>,
    /// The caller's own question that an approver rejected (`rejected_by_approver`), asked again
    /// here in new words or the same; the audit links the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<QuestionId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClarifyAnswer {
    /// The question that ended with the `clarify`.
    pub of: QuestionId,
    /// The `id` of the option picked.
    pub option: String,
}

/// `202` to `POST /v1/questions`, and the data of the `accepted` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Accepted {
    pub id: QuestionId,
    /// In the `accepted` event of a question asked again after a rejection: the rejected question
    /// its `follows` named. Left out otherwise, and always from the `202`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub follows: Option<QuestionId>,
}

impl Accepted {
    /// The `202`, or the `accepted` event of a question that follows none.
    pub fn new(id: QuestionId) -> Self {
        Self { id, follows: None }
    }
}

/// `GET /v1/questions/{id}`: where the question stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct QuestionState {
    pub id: QuestionId,
    pub status: QuestionStatus,
    #[schema(required = true)]
    pub project: Option<ProjectId>,
    #[schema(required = true)]
    pub class: Option<QuestionClass>,
    #[schema(required = true)]
    pub answer: Option<Answer>,
    #[schema(required = true)]
    pub refusal: Option<Refusal>,
    /// `null` until a judge has answered.
    #[schema(required = true)]
    pub judge_mode: Option<JudgeMode>,
    pub cost_usd: Usd,
    #[schema(required = true)]
    pub duration_ms: Option<u64>,
    #[schema(required = true)]
    pub audit_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Running,
    PendingSignoff,
    Delivered,
    Refused,
    Clarify,
    Failed,
}

/// An answer that passed the output checks, with the facts it cites. Also the `answer` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Answer {
    /// Plain text for the person, the citations removed.
    pub text: String,
    pub facts: Vec<Fact>,
    pub confidence: Ratio,
}

/// Why a question was refused, and what the person can do instead. Also the `refused` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Refusal {
    pub code: RefusalCode,
    pub message: String,
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// The class isn't in the effective policy.
    ForbiddenClass,
    /// The project isn't granted.
    ForbiddenProject,
    AsksForCode,
    AsksToEvaluatePeople,
    AsksForClientCommitment,
    ContainsAgentInstruction,
    /// An input check: contracts, prices, salaries, personal data, a client's internal information
    /// or security configuration.
    Confidential,
    /// The class is `other`.
    OutOfScope,
    /// The rules-only judge can't confirm a clean match.
    StrictMode,
    RejectedByApprover,
}

/// `POST /v1/questions/{id}/feedback`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Feedback {
    pub helpful: bool,
    pub needed_dev: bool,
}

/// `GET /v1/approvals?status=…`: the held answers of the approver's projects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ApprovalList {
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Approval {
    pub id: ApprovalId,
    pub question_id: QuestionId,
    /// `null` for a portfolio answer.
    #[schema(required = true)]
    pub project: Option<ProjectId>,
    pub class: QuestionClass,
    pub asker_role: Role,
    pub reason: SignoffReason,
    /// Exactly what approving delivers. Left out of the caller's own question: the asker reads
    /// nothing of a held answer before the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub draft: Option<Answer>,
    /// The draft's flagged sentences. Left out of the caller's own question, as the draft is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub flags: Option<Vec<Flag>>,
    /// The caller's own question, which they can't approve; it comes without `draft` and `flags`.
    pub asked_by_me: bool,
    pub status: ApprovalStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A sentence of the draft that a check flagged, with the check and its probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Flag {
    pub sentence: String,
    pub check: CheckId,
    pub p: Ratio,
}

/// Why an answer waits for an approver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignoffReason {
    /// The role or the manifest holds the class for sign-off.
    Class,
    /// An output check landed in its review band.
    ReviewBand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

/// `POST /v1/approvals/{id}`. A rejection needs a note, which the asker sees as the hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecision {
    pub decision: Decision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
}

/// `GET /v1/audit`: a page of audit rows, one per event, in `seq` order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AuditPage {
    pub items: Vec<AuditItem>,
    /// The cursor of the next page, `null` on the last one.
    #[schema(required = true)]
    pub next: Option<String>,
}

/// One audit row. `GET /v1/audit/export` writes the same rows, one JSON object per line. With
/// `payload_sha256` and `prev_hash` a row's `hash` can be recomputed without the database:
/// `SHA-256(prev_hash ‖ seq ‖ len(type) ‖ type ‖ payload_sha256 ‖ at)`, `seq` as 8 big-endian
/// bytes, `len(type)` as one byte, `at` written with milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AuditItem {
    pub seq: u64,
    #[schema(required = true)]
    pub question_id: Option<QuestionId>,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub at: DateTime<Utc>,
    #[schema(required = true)]
    pub user: Option<Email>,
    #[schema(required = true)]
    pub token: Option<TokenPublicId>,
    #[schema(required = true)]
    pub project: Option<ProjectId>,
    /// The event's data; `null` once retention deleted it. The hash still covers its digest.
    #[schema(required = true, value_type = Option<Object>)]
    pub payload: Option<serde_json::Value>,
    /// The SHA-256 of the payload's stored JSON, kept when retention deletes the payload.
    pub payload_sha256: Sha256Hex,
    /// The previous row's `hash`; 64 zeros before the first row.
    pub prev_hash: Sha256Hex,
    pub hash: Sha256Hex,
}

/// The body of every error: for requests that can't start. A question that starts and is then
/// refused ends with a `refused` event instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
    #[schema(required = true)]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthenticated,
    TokenExpired,
    ForbiddenProject,
    ForbiddenClass,
    /// With `Retry-After`.
    RateLimited,
    BudgetExceeded,
    ProbeUnavailable,
    JudgeUnavailable,
    InvalidRequest,
    Internal,
}

/// `POST /internal/probes/register`, from a probe on start and every 60 s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeRegistration {
    pub project: ProjectId,
    pub schema: SchemaId,
    /// The effective mode: the strictest of the token, the manifest and the admin's cap.
    pub access: AccessMode,
    /// What lowered the mode, or `null` when nothing did.
    #[serde(deserialize_with = "present")]
    #[schema(required = true)]
    pub access_limited_by: Option<AccessLimit>,
    /// The SHA-256 of the manifest the probe read.
    pub manifest_hash: Sha256Hex,
    pub manifest: ProbeManifest,
    pub tools: Vec<ToolName>,
}

/// Deserializes a field that must be present, even when it is `null`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_token_never_shows_in_debug() {
        let body: SessionRequest = serde_json::from_value(json!({ "token": "mcy_abc_secret" })).unwrap();
        assert_eq!(body.token.expose(), "mcy_abc_secret");
        let printed = format!("{body:?}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn requests_reject_unknown_fields() {
        assert!(serde_json::from_value::<SessionRequest>(json!({ "token": "t", "remember": true })).is_err());
        assert!(serde_json::from_value::<NewQuestion>(json!({ "text": "hi", "projects": "delta" })).is_err());
        assert!(serde_json::from_value::<Feedback>(json!({ "helpful": true })).is_err());
        assert!(
            serde_json::from_value::<ApprovalDecision>(json!({ "decision": "approve", "edit": "x" }))
                .is_err()
        );
    }

    #[test]
    fn responses_accept_added_fields() {
        let body = json!({ "status": "ok", "version": "0.0.0", "uptime_s": 12 });
        assert!(serde_json::from_value::<ServiceHealth>(body).is_ok());
    }

    #[test]
    fn a_question_needs_only_its_text() {
        let q: NewQuestion = serde_json::from_value(json!({ "text": "what's done?" })).unwrap();
        assert_eq!(q.project, None);
        assert_eq!(q.clarify, None);
        assert_eq!(q.follows, None);
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({ "text": "what's done?" })
        );
    }

    #[test]
    fn a_question_asked_again_names_the_rejected_one_and_its_acceptance_says_so() {
        let rejected = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y";
        let q: NewQuestion =
            serde_json::from_value(json!({ "text": "what's done?", "follows": rejected })).unwrap();
        assert_eq!(q.follows.as_ref().map(QuestionId::as_str), Some(rejected));
        assert!(serde_json::from_value::<NewQuestion>(json!({ "text": "hi", "follows": "42" })).is_err());

        let id = QuestionId::parse("q_01JB2M3N4P5Q6R7S8T9V0W1X2Z").unwrap();
        let plain = Accepted::new(id.clone());
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            json!({ "id": id.as_str() })
        );
        let again = Accepted {
            id: id.clone(),
            follows: q.follows,
        };
        assert_eq!(
            serde_json::to_value(&again).unwrap(),
            json!({ "id": id.as_str(), "follows": rejected })
        );
    }

    #[test]
    fn amounts_are_never_negative() {
        assert!(serde_json::from_value::<Usd>(json!(0.011)).is_ok());
        assert!(serde_json::from_value::<Usd>(json!(-0.5)).is_err());
        assert!(Usd::new(f64::NAN).is_none());
    }

    #[test]
    fn a_registration_states_its_limit_even_when_there_is_none() {
        let manifest = json!({
            "version": "0.1", "project": "delta", "display_name": "Delta", "allow": ["progress"],
            "judge_context": []
        });
        let mut body = json!({
            "project": "delta", "schema": "mercury.facts/0.1", "access": "metadata",
            "access_limited_by": null, "manifest_hash": "0".repeat(64), "manifest": manifest,
            "tools": ["describe"]
        });
        assert!(serde_json::from_value::<ProbeRegistration>(body.clone()).is_ok());
        body.as_object_mut().unwrap().remove("access_limited_by");
        assert!(serde_json::from_value::<ProbeRegistration>(body).is_err());
    }

    #[test]
    fn codes_travel_in_snake_case() {
        assert_eq!(
            serde_json::to_value(RefusalCode::RejectedByApprover).unwrap(),
            "rejected_by_approver"
        );
        assert_eq!(
            serde_json::to_value(RefusalCode::Confidential).unwrap(),
            "confidential"
        );
        assert_eq!(
            serde_json::from_value::<RefusalCode>(json!("confidential")).unwrap(),
            RefusalCode::Confidential
        );
        assert_eq!(
            serde_json::to_value(ErrorCode::TokenExpired).unwrap(),
            "token_expired"
        );
        assert_eq!(
            serde_json::to_value(QuestionStatus::PendingSignoff).unwrap(),
            "pending_signoff"
        );
        assert!(serde_json::from_value::<ErrorCode>(json!("not_found")).is_err());
    }
}
