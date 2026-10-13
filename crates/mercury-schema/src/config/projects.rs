//! `projects.yaml`: which probes exist, the admin's cap on each, and the copy of a manifest for
//! a probe that can't read its own.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};

use super::HttpUrl;
use crate::enums::{AccessMode, QuestionClass};
use crate::ids::{ModuleName, ProjectId};
use crate::manifest::{
    Deny, JudgeContext, ManifestVersion, ProbeManifest, TestsConfig, Tracker, deny_list, project_classes,
};
use crate::redact::RedactRule;

/// The id the pseudo-project for answers across projects uses; no project may take it.
pub const PORTFOLIO: &str = "portfolio";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsConfig {
    #[serde(deserialize_with = "projects")]
    pub projects: IndexMap<ProjectId, ProjectConfig>,
}

fn projects<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<IndexMap<ProjectId, ProjectConfig>, D::Error> {
    let projects = IndexMap::<ProjectId, ProjectConfig>::deserialize(deserializer)?;
    if projects.keys().any(|id| id.as_str() == PORTFOLIO) {
        return Err(serde::de::Error::custom(
            "`portfolio` is the pseudo-project for answers across projects; pick another id",
        ));
    }
    Ok(projects)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub display_name: String,
    /// Other names people use for the project, for the judge's project choice.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The admin's cap. The probe's effective mode is never above it.
    pub access_max: AccessMode,
    /// Used only when the probe can't read `.mercury/probe.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ManifestCopy>,
    pub probe: ProbeEndpoint,
}

/// Where the core reaches the project's probe, and the file holding the probe's token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeEndpoint {
    pub url: HttpUrl,
    pub token_file: PathBuf,
}

/// A manifest's rules without `version`, `project` and `display_name`, which the project's own
/// keys give. The keys and their checks are the manifest's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCopy {
    #[serde(default = "default_access")]
    pub access: AccessMode,
    #[serde(default)]
    pub modules: IndexMap<ModuleName, Vec<String>>,
    #[serde(default)]
    pub module_labels: IndexMap<ModuleName, Vec<String>>,
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
    pub judge_context: Vec<JudgeContext>,
}

fn default_access() -> AccessMode {
    AccessMode::Code
}

impl ManifestCopy {
    /// The manifest this copy stands for.
    pub fn into_manifest(self, project: ProjectId, display_name: String) -> ProbeManifest {
        ProbeManifest {
            version: ManifestVersion,
            project,
            display_name,
            access: self.access,
            modules: self.modules,
            module_labels: self.module_labels,
            exclude: self.exclude,
            tests: self.tests,
            tracker: self.tracker,
            blocked_labels: self.blocked_labels,
            allow: self.allow,
            deny: self.deny,
            redact: self.redact,
            signoff: self.signoff,
            judge_context: self.judge_context,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECTS: &str = r#"
projects:
  delta:
    display_name: "Delta — invoicing"
    aliases: [delta, invoicing, invoices]
    access_max: code
    probe:
      url: http://probe-delta:7000/mcp
      token_file: /run/mercury/probe-tokens/delta.token
  omega:
    display_name: "Omega — mobile app"
    aliases: [omega, mobile]
    access_max: metadata
    manifest:
      module_labels: { Login: [login], Payments: [payments] }
      blocked_labels: [blocked]
      allow: [progress, tested, blocked, health, changes]
      redact:
        - { pattern: '(?i)\bmegabank\b', replace: 'the client' }
      judge_context: [titles, labels]
    probe:
      url: http://probe-omega:7000/mcp
      token_file: /run/mercury/probe-tokens/omega.token
"#;

    fn parse(yaml: &str) -> Result<ProjectsConfig, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    #[test]
    fn the_documented_projects_parse() {
        let c = parse(PROJECTS).unwrap();
        let ids: Vec<_> = c.projects.keys().map(ProjectId::as_str).collect();
        assert_eq!(ids, ["delta", "omega"]);
        let omega = &c.projects[1];
        assert_eq!(omega.access_max, AccessMode::Metadata);
        assert_eq!(omega.probe.url.as_str(), "http://probe-omega:7000/mcp");
        let copy = omega.manifest.clone().unwrap();
        assert_eq!(copy.module_labels.len(), 2);
        let manifest = copy.into_manifest(ProjectId::parse("omega").unwrap(), omega.display_name.clone());
        assert_eq!(manifest.project.as_str(), "omega");
        assert_eq!(manifest.redact[0].replace, "the client");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse(&PROJECTS.replace("aliases: [omega", "alias: [omega")).is_err());
        assert!(parse(&PROJECTS.replace("blocked_labels: [blocked]", "blocked_label: [blocked]")).is_err());
        assert!(
            parse(&PROJECTS.replace("token_file: /run/mercury/probe-tokens/delta.token", "token: x"))
                .is_err()
        );
    }

    #[test]
    fn the_cap_is_required() {
        assert!(parse(&PROJECTS.replace("    access_max: code\n", "")).is_err());
    }

    #[test]
    fn the_copy_has_no_identity_of_its_own() {
        let with_project = PROJECTS.replace(
            "      blocked_labels:",
            "      project: omega\n      blocked_labels:",
        );
        assert!(parse(&with_project).is_err());
        let with_version = PROJECTS.replace(
            "      blocked_labels:",
            "      version: 0.1\n      blocked_labels:",
        );
        assert!(parse(&with_version).is_err());
    }

    #[test]
    fn the_copy_keeps_the_manifest_rules() {
        assert!(
            parse(&PROJECTS.replace("health, changes]\n      redact", "portfolio]\n      redact")).is_err()
        );
        assert!(parse(&PROJECTS.replace("      judge_context: [titles, labels]\n", "")).is_err());
        let open = parse(&PROJECTS.replace(
            "health, changes]\n      redact",
            "health, changes, open]\n      redact",
        ))
        .unwrap();
        let copy = open.projects[1].manifest.clone().unwrap();
        assert_eq!(copy.allow.last(), Some(&QuestionClass::Open));
    }

    #[test]
    fn portfolio_is_reserved() {
        assert!(parse(&PROJECTS.replace("  omega:", "  portfolio:")).is_err());
    }

    #[test]
    fn a_probe_address_is_checked() {
        assert!(parse(&PROJECTS.replace("http://probe-delta:7000/mcp", "probe-delta:7000")).is_err());
    }
}
