//! The core's configuration files in `/config`: `projects.yaml`, `policies.yaml`, `checks.yaml`
//! and `models.yaml`.
//!
//! Every type rejects unknown keys, so a typo fails loudly instead of silently turning a rule
//! off. A rule that needs one file is checked here, when the file is read; links between files
//! (a role's classes, a project's probe, a model role's backend) are `config check`'s.

mod checks;
mod models;
mod policies;
mod projects;

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use checks::{CheckConfig, CheckStage, CheckType, ChecksConfig, Polarity, Thresholds};
pub use models::{
    BackendName, Backends, Budgets, ChatRole, DataCollection, JudgeKind, JudgeRole, ModelRoles, ModelsConfig,
    ModelsError, OpenAiCompatibleBackend, OpenRouterBackend, ProviderSettings,
};
pub use policies::{GlobalPolicy, Limits, Policies, PoliciesVersion, PolicyDefaults, RolePolicy};
pub use projects::{ManifestCopy, PORTFOLIO, ProbeEndpoint, ProjectConfig, ProjectsConfig};

use crate::enums::QuestionClass;

/// An `http://` or `https://` address with a host. It names a service on the core's networks or
/// a model API; it carries no credentials (those come from files).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HttpUrl(String);

/// An address that isn't `http(s)://host…`, or that carries credentials. It never repeats the
/// address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid address: {0}")]
pub struct UrlError(&'static str);

impl HttpUrl {
    pub fn parse(raw: &str) -> Result<Self, UrlError> {
        let rest = raw
            .strip_prefix("https://")
            .or_else(|| raw.strip_prefix("http://"))
            .ok_or(UrlError("it must start with http:// or https://"))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() || authority.starts_with(':') {
            return Err(UrlError("it has no host"));
        }
        if authority.contains('@') {
            return Err(UrlError("credentials go in a file, never in the address"));
        }
        if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(UrlError("it contains a space or a control character"));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_https(&self) -> bool {
        self.0.starts_with("https://")
    }
}

impl fmt::Display for HttpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for HttpUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HttpUrl({})", self.0)
    }
}

impl Serialize for HttpUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for HttpUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// The classes a person can ask: the six project classes (the five and `open`) and `portfolio`.
/// Roles, sign-off and the global deny list name only these.
pub(crate) fn check_askable_class<E: serde::de::Error>(class: QuestionClass) -> Result<QuestionClass, E> {
    if QuestionClass::PROJECT.contains(&class) || class == QuestionClass::Portfolio {
        Ok(class)
    } else {
        Err(E::custom(
            "only progress, tested, blocked, health, changes, open and portfolio can be listed",
        ))
    }
}

pub(crate) fn askable_classes<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<QuestionClass>, D::Error> {
    Vec::<QuestionClass>::deserialize(deserializer)?
        .into_iter()
        .map(check_askable_class)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_http_with_a_host_and_no_credentials() {
        assert!(HttpUrl::parse("http://probe-delta:7000/mcp").is_ok());
        assert!(
            HttpUrl::parse("https://openrouter.ai/api/v1/chat/completions")
                .unwrap()
                .is_https()
        );
        for bad in [
            "probe-delta:7000/mcp",
            "ftp://example.com",
            "http://",
            "http://:7000/mcp",
            "https://user:key@example.com/v1",
            "http://example.com/a b",
        ] {
            assert!(HttpUrl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_address_error_never_repeats_the_address() {
        let err = HttpUrl::parse("https://user:hunter2@example.com").unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }
}
