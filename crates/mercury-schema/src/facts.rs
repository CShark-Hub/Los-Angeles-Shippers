//! Facts and the probe's tool replies (`mercury.facts/0.1`).
//!
//! Every struct here rejects unknown fields, and every value is an enum, a number, a date, an
//! identifier with a fixed shape, a [`SafeText`] or a [`DisplayName`]. There is no `String`.
//! A nullable field is still required: `null` says the probe can't know, a missing field is an
//! error.

use std::borrow::Cow;

use chrono::{DateTime, NaiveDate, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

use crate::SCHEMA;
use crate::api::Usd;
use crate::enums::{AccessLimit, AccessMode, ToolName};
use crate::ids::{CommitShort, FactId, IssueNumber, ModuleName, ProjectId};
use crate::redact::{DisplayName, SafeText};

/// Deserializes a field that must be present, even when it is `null`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

/// The constant `mercury.facts/0.1`. Any other value is rejected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaId;

impl Serialize for SchemaId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(SCHEMA)
    }
}

impl<'de> Deserialize<'de> for SchemaId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == SCHEMA {
            Ok(SchemaId)
        } else {
            Err(serde::de::Error::custom("unsupported facts schema"))
        }
    }
}

impl JsonSchema for SchemaId {
    fn schema_name() -> Cow<'static, str> {
        "SchemaId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "const": SCHEMA })
    }
}

/// A number from 0 to 1: a confidence, a pass rate or a share.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Ratio(f64);

impl Ratio {
    pub fn new(value: f64) -> Option<Self> {
        (0.0..=1.0).contains(&value).then_some(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for Ratio {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for Ratio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = f64::deserialize(deserializer)?;
        Ratio::new(raw).ok_or_else(|| serde::de::Error::custom("a ratio is between 0 and 1"))
    }
}

impl JsonSchema for Ratio {
    fn schema_name() -> Cow<'static, str> {
        "Ratio".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "number", "minimum": 0, "maximum": 1 })
    }
}

/// A health score from 0 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Score(#[schemars(range(max = 100))] u8);

impl Score {
    pub fn new(value: u8) -> Option<Self> {
        (value <= 100).then_some(Self(value))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u8::deserialize(deserializer)?;
        Score::new(raw).ok_or_else(|| serde::de::Error::custom("a score is between 0 and 100"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Feature,
    Fix,
    Chore,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    InReview,
    InProgress,
    Blocked,
    NotStarted,
    Unclear,
}

/// `missing` only where the probe counts tests, never in `metadata` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tested {
    Passing,
    Failing,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitingOn {
    Review,
    Dependency,
    Environment,
    External,
    Decision,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Pr,
    Issue,
    Ci,
    Release,
}

/// The states of every evidence type in one list; [`Evidence`] checks that the pair fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Open,
    Draft,
    Merged,
    Closed,
    Passed,
    Failed,
    Running,
    Published,
}

/// What a fact rests on. Deserializing rejects a state that doesn't fit the type, and a `ref` on
/// CI runs or releases.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    #[serde(rename = "type")]
    pub kind: EvidenceType,
    /// `#<number>` for pull requests and issues when the policy shows references, else `null`.
    #[schema(required = true)]
    #[serde(rename = "ref")]
    pub reference: Option<IssueNumber>,
    pub state: EvidenceState,
    pub at: NaiveDate,
}

impl Evidence {
    fn fits(&self) -> bool {
        use EvidenceState as S;
        use EvidenceType as T;
        match self.kind {
            T::Pr => matches!(self.state, S::Open | S::Draft | S::Merged | S::Closed),
            T::Issue => matches!(self.state, S::Open | S::Closed),
            T::Ci => matches!(self.state, S::Passed | S::Failed | S::Running) && self.reference.is_none(),
            T::Release => self.state == S::Published && self.reference.is_none(),
        }
    }
}

impl<'de> Deserialize<'de> for Evidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            kind: EvidenceType,
            #[serde(rename = "ref")]
            #[serde(deserialize_with = "present")]
            reference: Option<IssueNumber>,
            state: EvidenceState,
            at: NaiveDate,
        }
        let Wire {
            kind,
            reference,
            state,
            at,
        } = Wire::deserialize(deserializer)?;
        let evidence = Evidence {
            kind,
            reference,
            state,
            at,
        };
        if evidence.fits() {
            Ok(evidence)
        } else {
            Err(serde::de::Error::custom(
                "the evidence state or ref doesn't fit its type",
            ))
        }
    }
}

/// One structured statement about a piece of work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub id: FactId,
    pub kind: Kind,
    pub title: SafeText,
    pub module: ModuleName,
    pub status: Status,
    pub tested: Tested,
    pub confidence: Ratio,
    #[schema(required = true)]
    #[serde(deserialize_with = "present")]
    pub waiting_on: Option<WaitingOn>,
    /// Only when the effective policy shows people; otherwise empty.
    pub people: Vec<DisplayName>,
    pub evidence: Vec<Evidence>,
    /// `true` when the title failed a rule or a check and was replaced.
    pub title_withheld: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Passed,
    Failed,
    Running,
    None,
}

/// The latest CI run: `at` is `null` when the status is `none`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiRun {
    pub status: BuildStatus,
    #[serde(deserialize_with = "present")]
    pub at: Option<DateTime<Utc>>,
}

/// The snapshot the index was built from. `commit_short` is `null` in `metadata` mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    #[serde(deserialize_with = "present")]
    pub commit_short: Option<CommitShort>,
    pub indexed_at: DateTime<Utc>,
    pub meta_at: DateTime<Utc>,
}

/// The reply of `describe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeReply {
    pub schema: SchemaId,
    pub project: ProjectId,
    pub display_name: SafeText,
    /// The effective mode: the strictest of the token, the manifest and the admin's cap.
    pub access: AccessMode,
    /// What lowered the mode, or `null` when nothing did.
    #[serde(deserialize_with = "present")]
    pub access_limited_by: Option<AccessLimit>,
    pub modules: Vec<ModuleName>,
    pub tools: Vec<ToolName>,
    pub snapshot: Snapshot,
}

/// The reply of `progress`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressReply {
    pub schema: SchemaId,
    pub as_of: DateTime<Utc>,
    pub items: Vec<Fact>,
}

/// The tests and CI results of one module.
/// The counts are `null` in `metadata` mode, where tests aren't counted, which is not zero tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModuleStats {
    pub module: ModuleName,
    #[serde(deserialize_with = "present")]
    pub tests_total: Option<u32>,
    #[serde(deserialize_with = "present")]
    pub tests_changed_14d: Option<u32>,
    pub last_ci: CiRun,
    pub pass_rate_7d: Ratio,
    /// The share of flaky CI runs in the last 7 days.
    pub flaky_7d: Ratio,
}

/// The reply of `tested`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestedReply {
    pub schema: SchemaId,
    pub as_of: DateTime<Utc>,
    pub modules: Vec<ModuleStats>,
}

/// The reply of `blocked`: facts with the status `blocked` and what they wait on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockedReply {
    pub schema: SchemaId,
    pub as_of: DateTime<Utc>,
    pub items: Vec<Fact>,
}

/// The most findings one `ask_code` reply holds.
pub const MAX_FINDINGS: usize = 12;

/// The reply of `ask_code`, served only in `code_model`: what the project's code reader found in
/// the snapshot for one question. The reader's words reach the core only as `SafeText`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskCodeReply {
    pub schema: SchemaId,
    /// When the snapshot the findings were read from was taken.
    pub as_of: DateTime<Utc>,
    /// At most 12.
    #[serde(deserialize_with = "at_most_twelve")]
    #[schemars(length(max = MAX_FINDINGS))]
    pub findings: Vec<Finding>,
    /// Findings dropped because their text failed the `SafeText` rules.
    pub withheld: u32,
    /// What the code reader's model cost for this question.
    pub cost_usd: Usd,
    /// The version of the code reader's prompt.
    pub prompt_version: u16,
}

/// One thing the code reader found, in a line anyone may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub text: SafeText,
    /// The probe's module the finding is about, or `null` when it is about none in particular.
    #[serde(deserialize_with = "present")]
    pub module: Option<ModuleName>,
    /// How sure the reader is, from 0 to 100.
    #[serde(deserialize_with = "up_to_100")]
    #[schemars(range(max = 100))]
    pub confidence: u8,
}

fn at_most_twelve<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Finding>, D::Error> {
    let findings = Vec::<Finding>::deserialize(deserializer)?;
    if findings.len() > MAX_FINDINGS {
        Err(serde::de::Error::custom("a reply holds at most 12 findings"))
    } else {
        Ok(findings)
    }
}

fn up_to_100<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let raw = u8::deserialize(deserializer)?;
    if raw <= 100 {
        Ok(raw)
    } else {
        Err(serde::de::Error::custom("a confidence is between 0 and 100"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenPrs {
    pub count: u32,
    pub oldest_days: u32,
}

/// TODO and FIXME markers. `null` as a whole in `metadata` mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoFixme {
    pub count: u32,
    /// The change in the count over the last 14 days, negative when it fell.
    pub trend_14d: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LastRelease {
    #[serde(deserialize_with = "present")]
    pub name: Option<SafeText>,
    #[serde(deserialize_with = "present")]
    pub at: Option<DateTime<Utc>>,
}

/// The reply of `health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthReply {
    pub schema: SchemaId,
    pub as_of: DateTime<Utc>,
    pub build: CiRun,
    pub ci_pass_rate_7d: Ratio,
    pub open_prs: OpenPrs,
    #[serde(deserialize_with = "present")]
    pub todo_fixme: Option<TodoFixme>,
    pub last_release: LastRelease,
    pub score: Score,
}

/// What changed in one module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeSummary {
    pub module: ModuleName,
    pub commits: u32,
    pub merged_prs: u32,
    pub notable: Vec<SafeText>,
}

/// The reply of `changes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangesReply {
    pub schema: SchemaId,
    pub as_of: DateTime<Utc>,
    pub modules: Vec<ChangeSummary>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn fact_json() -> Value {
        json!({
            "id": "f_7Qx2",
            "kind": "feature",
            "title": "Invoice list with filters",
            "module": "Invoicing",
            "status": "done",
            "tested": "passing",
            "confidence": 0.93,
            "waiting_on": null,
            "people": [],
            "evidence": [
                { "type": "pr", "ref": "#412", "state": "merged", "at": "2026-10-04" },
                { "type": "ci", "ref": null, "state": "passed", "at": "2026-10-04" }
            ],
            "title_withheld": false,
            "updated_at": "2026-10-04T15:12:00Z"
        })
    }

    #[test]
    fn the_spec_example_parses_and_round_trips() {
        let fact: Fact = serde_json::from_value(fact_json()).unwrap();
        assert_eq!(fact.status, Status::Done);
        assert_eq!(serde_json::to_value(&fact).unwrap(), fact_json());
    }

    #[test]
    fn an_extra_field_is_rejected() {
        let mut v = fact_json();
        v["details"] = json!("fn export() {}");
        assert!(serde_json::from_value::<Fact>(v).is_err());
    }

    #[test]
    fn a_missing_nullable_field_is_rejected() {
        let mut v = fact_json();
        v.as_object_mut().unwrap().remove("waiting_on");
        assert!(serde_json::from_value::<Fact>(v).is_err());
        let mut v = fact_json();
        v["evidence"][1].as_object_mut().unwrap().remove("ref");
        assert!(serde_json::from_value::<Fact>(v).is_err());
        let snapshot = json!({ "indexed_at": "2026-10-04T15:12:00Z", "meta_at": "2026-10-04T15:10:00Z" });
        assert!(serde_json::from_value::<Snapshot>(snapshot).is_err());
    }

    #[test]
    fn raw_text_in_a_title_is_rejected() {
        let mut v = fact_json();
        v["title"] = json!("See src/Export/CsvWriter.cs");
        assert!(serde_json::from_value::<Fact>(v).is_err());
    }

    #[test]
    fn numbers_out_of_range_are_rejected() {
        let mut v = fact_json();
        v["confidence"] = json!(1.5);
        assert!(serde_json::from_value::<Fact>(v).is_err());
        assert!(serde_json::from_value::<Score>(json!(101)).is_err());
        assert!(serde_json::from_value::<Score>(json!(100)).is_ok());
        assert!(serde_json::from_value::<Ratio>(json!(-0.1)).is_err());
    }

    #[test]
    fn a_wrong_schema_id_is_rejected() {
        let reply = json!({ "schema": "mercury.facts/9", "as_of": "2026-10-04T15:12:00Z", "items": [] });
        assert!(serde_json::from_value::<ProgressReply>(reply).is_err());
    }

    fn ask_code_json() -> Value {
        json!({
            "schema": "mercury.facts/0.1",
            "as_of": "2026-10-04T15:12:30Z",
            "findings": [
                { "text": "The export writes one file per invoice batch", "module": "Export",
                  "confidence": 82 },
                { "text": "Totals are rounded once, after the discounts", "module": null,
                  "confidence": 64 }
            ],
            "withheld": 1,
            "cost_usd": 0.0031,
            "prompt_version": 1
        })
    }

    #[test]
    fn an_ask_code_reply_parses_and_round_trips() {
        let reply: AskCodeReply = serde_json::from_value(ask_code_json()).unwrap();
        assert_eq!(reply.findings.len(), 2);
        assert_eq!(reply.findings[1].module, None);
        assert_eq!(reply.withheld, 1);
        assert_eq!(serde_json::to_value(&reply).unwrap(), ask_code_json());
        let mut v = ask_code_json();
        v["answer"] = json!("The export is in the exporter class.");
        assert!(serde_json::from_value::<AskCodeReply>(v).is_err());
        let mut v = ask_code_json();
        v["cost_usd"] = json!(-0.01);
        assert!(serde_json::from_value::<AskCodeReply>(v).is_err());
    }

    #[test]
    fn an_ask_code_reply_holds_at_most_twelve_findings() {
        let finding = json!({ "text": "Totals are rounded once", "module": null, "confidence": 50 });
        let with = |n: usize| {
            let mut v = ask_code_json();
            v["findings"] = Value::Array(vec![finding.clone(); n]);
            serde_json::from_value::<AskCodeReply>(v)
        };
        assert!(with(0).is_ok());
        assert!(with(MAX_FINDINGS).is_ok());
        assert!(with(MAX_FINDINGS + 1).is_err());
        let schema = schemars::schema_for!(AskCodeReply).to_value();
        assert_eq!(schema["properties"]["findings"]["maxItems"], 12, "{schema}");
    }

    #[test]
    fn a_finding_is_safe_text_with_a_confidence_up_to_100() {
        let read = |v: Value| serde_json::from_value::<Finding>(v);
        let finding = read(json!({ "text": "Totals are rounded once", "module": null, "confidence": 100 }));
        assert_eq!(finding.unwrap().module, None);
        assert!(
            read(json!({ "text": "See src/Export/CsvWriter.cs", "module": "Export", "confidence": 70 }))
                .is_err()
        );
        assert!(
            read(json!({ "text": "Totals are rounded once", "module": null, "confidence": 101 })).is_err()
        );
        assert!(read(json!({ "text": "Totals are rounded once", "confidence": 70 })).is_err());
        let schema = schemars::schema_for!(Finding).to_value();
        assert_eq!(schema["properties"]["confidence"]["maximum"], 100, "{schema}");
    }

    #[test]
    fn evidence_must_fit_its_type() {
        let ok = |v: Value| serde_json::from_value::<Evidence>(v).is_ok();
        assert!(ok(
            json!({ "type": "issue", "ref": "#7", "state": "closed", "at": "2026-10-04" })
        ));
        assert!(ok(
            json!({ "type": "release", "ref": null, "state": "published", "at": "2026-10-04" })
        ));
        assert!(!ok(
            json!({ "type": "ci", "ref": "#9", "state": "passed", "at": "2026-10-04" })
        ));
        assert!(!ok(
            json!({ "type": "issue", "ref": "#7", "state": "merged", "at": "2026-10-04" })
        ));
        assert!(!ok(
            json!({ "type": "release", "ref": null, "state": "open", "at": "2026-10-04" })
        ));
        assert!(!ok(
            json!({ "type": "pr", "ref": "#7", "state": "open", "at": "2026-10-04", "branch": "x" })
        ));
    }
}
