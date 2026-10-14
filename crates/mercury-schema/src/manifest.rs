//! `.mercury/probe.yaml`: the repository's own rules, reviewed by its owner like code.
//!
//! Every type rejects unknown keys, so a typo fails loudly instead of silently turning a rule off.

use std::borrow::Cow;

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::enums::{AccessMode, QuestionClass};
use crate::ids::{ModuleName, ProjectId};
use crate::redact::RedactRule;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeManifest {
    pub version: ManifestVersion,
    pub project: ProjectId,
    pub display_name: String,
    /// Can only lower what the token and the admin's cap allow.
    #[serde(default = "default_access")]
    pub access: AccessMode,
    /// Globs that map files to modules, in the order they are tried.
    #[serde(default)]
    pub modules: IndexMap<ModuleName, Vec<String>>,
    /// Labels that put pull requests and issues into a module; the only mapping in `metadata` mode.
    #[serde(default)]
    pub module_labels: IndexMap<ModuleName, Vec<String>>,
    /// Globs deleted right after a snapshot is unpacked, never indexed.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub tests: TestsConfig,
    #[serde(default)]
    pub tracker: Tracker,
    #[serde(default)]
    pub blocked_labels: Vec<String>,
    #[serde(deserialize_with = "project_classes")]
    pub allow: Vec<QuestionClass>,
    #[serde(default, deserialize_with = "deny_list")]
    pub deny: Vec<Deny>,
    #[serde(default)]
    pub redact: Vec<RedactRule>,
    #[serde(default, deserialize_with = "project_classes")]
    pub signoff: Vec<QuestionClass>,
    /// Which repository texts the judge may see. No default: the owner states it.
    pub judge_context: Vec<JudgeContext>,
}

fn default_access() -> AccessMode {
    AccessMode::Code
}

/// A manifest lists only the classes a single project answers.
fn check_project_class<E: serde::de::Error>(class: QuestionClass) -> Result<QuestionClass, E> {
    if QuestionClass::PROJECT.contains(&class) {
        Ok(class)
    } else {
        Err(E::custom(
            "a manifest lists only progress, tested, blocked, health, changes and open",
        ))
    }
}

pub(crate) fn project_classes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<QuestionClass>, D::Error> {
    Vec::<QuestionClass>::deserialize(deserializer)?
        .into_iter()
        .map(check_project_class)
        .collect()
}

pub(crate) fn deny_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Deny>, D::Error> {
    Vec::<Deny>::deserialize(deserializer)?
        .into_iter()
        .map(|deny| match deny {
            Deny::Class(class) => check_project_class(class).map(Deny::Class),
            topic => Ok(topic),
        })
        .collect()
}

/// Accepts `0.1` written as a number or a string; only `0.1` is supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub struct ManifestVersion;

impl From<ManifestVersion> for String {
    fn from(_: ManifestVersion) -> Self {
        "0.1".to_owned()
    }
}

impl<'de> Deserialize<'de> for ManifestVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Number(f64),
            Text(String),
        }
        let supported = match Wire::deserialize(deserializer)? {
            Wire::Number(n) => (n - 0.1).abs() < f64::EPSILON,
            Wire::Text(s) => s == "0.1",
        };
        if supported {
            Ok(ManifestVersion)
        } else {
            Err(serde::de::Error::custom("only manifest version 0.1 is supported"))
        }
    }
}

/// Described as it is written out: the string `0.1`.
impl JsonSchema for ManifestVersion {
    fn schema_name() -> Cow<'static, str> {
        "ManifestVersion".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "const": "0.1" })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestsConfig {
    #[serde(default)]
    pub frameworks: Vec<TestFramework>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestFramework {
    Rust,
    Jest,
    Vitest,
    Xunit,
    Nunit,
    Mstest,
    Junit,
    Pytest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tracker {
    #[default]
    GithubIssues,
}

/// What a project refuses: a whole question class, or a topic the checks look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(untagged)]
pub enum Deny {
    Class(QuestionClass),
    Topic(DenyTopic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DenyTopic {
    SourceCode,
    FilePaths,
    Secrets,
    PeopleEvaluation,
    ClientCommitments,
}

/// Repository texts the judge may see as context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgeContext {
    Titles,
    Labels,
    Comments,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
version: 0.1
project: delta
display_name: "Delta — invoicing"
access: code
modules:
  Invoicing: ["src/Invoices/**", "src/Billing/**"]
  Export:    ["src/Export/**"]
  Auth:      ["src/Auth/**"]
module_labels:
  Invoicing: [invoicing]
  Export:    [export]
exclude: ["docs/customers/**", "**/*.pem", "deploy/secrets/**"]
tests:
  frameworks: [xunit]
tracker: github_issues
blocked_labels: [blocked, waiting]
allow: [progress, tested, blocked, health, changes]
deny: []
redact:
  - { pattern: '(?i)\bmegabank\b', replace: 'the client' }
signoff: []
judge_context: [titles, labels]
"#;

    fn parse(yaml: &str) -> Result<ProbeManifest, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    #[test]
    fn the_documented_manifest_parses() {
        let m = parse(MANIFEST).unwrap();
        assert_eq!(m.project.as_str(), "delta");
        assert_eq!(m.access, AccessMode::Code);
        let modules: Vec<_> = m.modules.keys().map(ModuleName::as_str).collect();
        assert_eq!(modules, ["Invoicing", "Export", "Auth"]);
        assert_eq!(m.module_labels.len(), 2);
        assert_eq!(m.exclude.len(), 3);
        assert_eq!(m.redact[0].replace, "the client");
    }

    #[test]
    fn unknown_keys_and_versions_are_rejected() {
        assert!(parse(&MANIFEST.replace("blocked_labels", "blocked_label")).is_err());
        assert!(parse(&MANIFEST.replace("version: 0.1", "version: 0.2")).is_err());
        assert!(parse(&MANIFEST.replace("version: 0.1", "version: \"0.1\"")).is_ok());
        assert!(parse(&MANIFEST.replace("access: code", "access: everything")).is_err());
    }

    #[test]
    fn module_names_are_checked() {
        assert!(parse(&MANIFEST.replace("Auth:      [", "src/Auth:  [")).is_err());
    }

    #[test]
    fn only_project_classes_may_be_listed() {
        assert!(parse(&MANIFEST.replace("health, changes]", "health, changes, portfolio]")).is_err());
        assert!(parse(&MANIFEST.replace("signoff: []", "signoff: [other]")).is_err());
        assert!(parse(&MANIFEST.replace("deny: []", "deny: [portfolio]")).is_err());
        let open = parse(
            &MANIFEST
                .replace("health, changes]", "health, changes, open]")
                .replace("signoff: []", "signoff: [open]")
                .replace("deny: []", "deny: [open]"),
        )
        .unwrap();
        assert_eq!(open.allow.last(), Some(&QuestionClass::Open));
        assert_eq!(open.signoff, [QuestionClass::Open]);
        assert_eq!(open.deny, [Deny::Class(QuestionClass::Open)]);
    }

    #[test]
    fn deny_takes_classes_and_topics() {
        let m = parse(&MANIFEST.replace("deny: []", "deny: [tested, people_evaluation]")).unwrap();
        assert_eq!(
            m.deny,
            [
                Deny::Class(QuestionClass::Tested),
                Deny::Topic(DenyTopic::PeopleEvaluation)
            ]
        );
        assert!(parse(&MANIFEST.replace("deny: []", "deny: [salaries]")).is_err());
    }

    #[test]
    fn access_defaults_to_code_and_judge_context_has_no_default() {
        let m = parse(&MANIFEST.replace("access: code\n", "")).unwrap();
        assert_eq!(m.access, AccessMode::Code);
        assert!(parse(&MANIFEST.replace("judge_context: [titles, labels]\n", "")).is_err());
    }

    #[test]
    fn a_manifest_survives_a_json_round_trip() {
        let m = parse(MANIFEST).unwrap();
        let back: ProbeManifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
