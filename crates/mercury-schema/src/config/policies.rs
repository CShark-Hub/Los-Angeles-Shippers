//! `policies.yaml`: what each role may ask, what is refused and redacted everywhere, and the
//! limits.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};

use super::askable_classes;
use crate::enums::{QuestionClass, Role};
use crate::manifest::Deny;
use crate::redact::RedactRule;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policies {
    pub version: PoliciesVersion,
    pub defaults: PolicyDefaults,
    /// A role missing from the file may ask nothing.
    pub roles: IndexMap<Role, RolePolicy>,
    pub global: GlobalPolicy,
    pub limits: Limits,
    /// How long the audit keeps what it can let go; 90 days when left out.
    #[serde(default)]
    pub retention: Retention,
}

/// Only version 1 exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "u8")]
pub struct PoliciesVersion;

impl From<PoliciesVersion> for u8 {
    fn from(_: PoliciesVersion) -> Self {
        1
    }
}

impl<'de> Deserialize<'de> for PoliciesVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u64::deserialize(deserializer)? {
            1 => Ok(PoliciesVersion),
            _ => Err(serde::de::Error::custom("only policies version 1 is supported")),
        }
    }
}

/// What answers show unless a role says otherwise. Every key is required: no privacy setting
/// falls back to a value nobody wrote down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDefaults {
    /// `#412` on pull request and issue evidence.
    pub show_evidence_refs: bool,
    /// The people on a fact.
    pub show_people: bool,
    /// `false` withholds every repository title before any model sees it.
    pub show_titles: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePolicy {
    #[serde(deserialize_with = "askable_classes")]
    pub classes: Vec<QuestionClass>,
    /// Overrides `defaults.show_people` for this role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_people: Option<bool>,
    /// Only numbers across modules, never single items.
    #[serde(default)]
    pub aggregates_only: bool,
    /// Classes whose answers wait for an approver.
    #[serde(default, deserialize_with = "askable_classes")]
    pub signoff: Vec<QuestionClass>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalPolicy {
    /// Refused for every project, on top of each manifest's own list.
    #[serde(default, deserialize_with = "deny_list")]
    pub deny: Vec<Deny>,
    /// Applied before each manifest's own rules.
    #[serde(default)]
    pub redact: Vec<RedactRule>,
    /// Hold answers with a review-band sentence for an approver, where the project has one.
    pub hold_review_band_for_signoff: bool,
}

fn deny_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Deny>, D::Error> {
    Vec::<Deny>::deserialize(deserializer)?
        .into_iter()
        .map(|deny| match deny {
            Deny::Class(class) => super::check_askable_class(class).map(Deny::Class),
            topic => Ok(topic),
        })
        .collect()
}

/// Questions per token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(deserialize_with = "at_least_one")]
    pub questions_per_hour: u32,
    #[serde(deserialize_with = "at_least_one")]
    pub burst: u32,
}

/// What retention keeps. The rows, their digests and their hashes stay forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    /// Days an audit row keeps its payload, and a question its text and answer; at least 1.
    #[serde(default = "ninety_days", deserialize_with = "at_least_one_day")]
    pub audit_payload_days: u32,
}

/// The window when `retention` is left out.
pub const AUDIT_PAYLOAD_DAYS: u32 = 90;

fn ninety_days() -> u32 {
    AUDIT_PAYLOAD_DAYS
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            audit_payload_days: AUDIT_PAYLOAD_DAYS,
        }
    }
}

fn at_least_one_day<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    match u32::deserialize(deserializer)? {
        0 => Err(serde::de::Error::custom(
            "a retention of 0 days deletes every payload at once",
        )),
        n => Ok(n),
    }
}

fn at_least_one<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    match u32::deserialize(deserializer)? {
        0 => Err(serde::de::Error::custom("a limit of 0 refuses every question")),
        n => Ok(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DenyTopic;

    const POLICIES: &str = r#"
version: 1
defaults:
  show_evidence_refs: true
  show_people: false
  show_titles: true
roles:
  pm:
    classes: [progress, tested, blocked, health, changes]
    show_people: true
  board:
    classes: [health, portfolio]
    aggregates_only: true
    signoff: [portfolio]
global:
  deny: [source_code, file_paths]
  redact:
    - { pattern: '(?i)\bacme\b', replace: 'the client' }
  hold_review_band_for_signoff: true
limits:
  questions_per_hour: 30
  burst: 5
"#;

    fn parse(yaml: &str) -> Result<Policies, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    #[test]
    fn the_documented_policies_parse() {
        let p = parse(POLICIES).unwrap();
        assert!(p.defaults.show_titles);
        assert_eq!(p.roles[&Role::Pm].show_people, Some(true));
        assert!(p.roles[&Role::Board].aggregates_only);
        assert_eq!(p.roles[&Role::Board].signoff, [QuestionClass::Portfolio]);
        assert_eq!(p.global.deny[0], Deny::Topic(DenyTopic::SourceCode));
        assert_eq!(p.limits.burst, 5);
    }

    #[test]
    fn unknown_keys_and_versions_are_rejected() {
        assert!(parse(&POLICIES.replace("burst: 5", "burst: 5\n  per_day: 100")).is_err());
        assert!(parse(&POLICIES.replace("aggregates_only", "aggregate_only")).is_err());
        assert!(parse(&POLICIES.replace("version: 1", "version: 2")).is_err());
        assert!(parse(&POLICIES.replace("  pm:", "  intern:")).is_err());
    }

    #[test]
    fn privacy_settings_must_be_written_down() {
        assert!(parse(&POLICIES.replace("  show_titles: true\n", "")).is_err());
        assert!(parse(&POLICIES.replace("  hold_review_band_for_signoff: true\n", "")).is_err());
    }

    #[test]
    fn only_askable_classes_are_listed() {
        assert!(parse(&POLICIES.replace("[health, portfolio]", "[health, other]")).is_err());
        assert!(parse(&POLICIES.replace("signoff: [portfolio]", "signoff: [forbidden]")).is_err());
        assert!(parse(&POLICIES.replace("deny: [source_code, file_paths]", "deny: [portfolio]")).is_ok());
        assert!(parse(&POLICIES.replace("deny: [source_code, file_paths]", "deny: [other]")).is_err());
        let open = parse(
            &POLICIES
                .replace("[health, portfolio]", "[health, open, portfolio]")
                .replace("signoff: [portfolio]", "signoff: [open]")
                .replace("deny: [source_code, file_paths]", "deny: [open]"),
        )
        .unwrap();
        assert_eq!(
            open.roles[&Role::Board].classes,
            [
                QuestionClass::Health,
                QuestionClass::Open,
                QuestionClass::Portfolio
            ]
        );
        assert_eq!(open.roles[&Role::Board].signoff, [QuestionClass::Open]);
        assert_eq!(open.global.deny, [Deny::Class(QuestionClass::Open)]);
    }

    #[test]
    fn a_zero_limit_is_refused() {
        assert!(parse(&POLICIES.replace("burst: 5", "burst: 0")).is_err());
    }

    #[test]
    fn retention_keeps_ninety_days_unless_told_otherwise() {
        assert_eq!(parse(POLICIES).unwrap().retention.audit_payload_days, 90);
        let with = |days: &str| format!("{POLICIES}retention:\n  audit_payload_days: {days}\n");
        assert_eq!(parse(&with("30")).unwrap().retention.audit_payload_days, 30);
        let zero = parse(&with("0")).unwrap_err().to_string();
        assert!(zero.contains("0 days"), "{zero}");
        assert!(parse(&format!("{POLICIES}retention:\n  payload_days: 30\n")).is_err());
        assert_eq!(
            parse(&format!("{POLICIES}retention: {{}}\n"))
                .unwrap()
                .retention
                .audit_payload_days,
            90
        );
    }
}
