//! `models.yaml`: the model backends, which backend and model each of the core's roles uses, and
//! the budgets.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use super::HttpUrl;
use crate::api::Usd;
use crate::ids::ModelSlug;

/// Checked when read: every role names a backend the file configures, `jev` runs only on
/// `openrouter` (Jev is cloud-only), and the fallback judge is a chat judge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Unchecked")]
pub struct ModelsConfig {
    pub backends: Backends,
    pub roles: ModelRoles,
    pub budgets: Budgets,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unchecked {
    backends: Backends,
    roles: ModelRoles,
    budgets: Budgets,
}

/// A rule of `models.yaml` the file breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelsError {
    #[error("the role `{role}` uses the backend `{backend}`, which the file doesn't configure")]
    MissingBackend {
        role: &'static str,
        backend: &'static str,
    },
    #[error("the role `{0}` is a Jev judge, which runs only on the openrouter backend")]
    JevOffOpenRouter(&'static str),
    #[error("the fallback judge is a chat judge: `kind: chat`")]
    FallbackNotChat,
}

impl TryFrom<Unchecked> for ModelsConfig {
    type Error = ModelsError;

    fn try_from(raw: Unchecked) -> Result<Self, Self::Error> {
        let Unchecked {
            backends,
            roles,
            budgets,
        } = raw;
        for (role, backend) in roles.backends() {
            if !backends.has(backend) {
                return Err(ModelsError::MissingBackend {
                    role,
                    backend: backend.as_str(),
                });
            }
        }
        for (role, judge) in [
            ("judge", Some(&roles.judge)),
            ("judge_fallback", roles.judge_fallback.as_ref()),
        ] {
            if let Some(judge) = judge
                && judge.kind == JudgeKind::Jev
                && judge.backend != BackendName::Openrouter
            {
                return Err(ModelsError::JevOffOpenRouter(role));
            }
        }
        if roles
            .judge_fallback
            .as_ref()
            .is_some_and(|j| j.kind != JudgeKind::Chat)
        {
            return Err(ModelsError::FallbackNotChat);
        }
        Ok(Self {
            backends,
            roles,
            budgets,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackendName {
    /// One key held by the core; the chat API and Jev's Decisions API.
    Openrouter,
    /// A model server with the OpenAI chat API that your organisation runs itself.
    OpenaiCompatible,
}

impl BackendName {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendName::Openrouter => "openrouter",
            BackendName::OpenaiCompatible => "openai_compatible",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backends {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter: Option<OpenRouterBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_compatible: Option<OpenAiCompatibleBackend>,
}

impl Backends {
    fn has(&self, name: BackendName) -> bool {
        match name {
            BackendName::Openrouter => self.openrouter.is_some(),
            BackendName::OpenaiCompatible => self.openai_compatible.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterBackend {
    pub chat_url: HttpUrl,
    pub decisions_url: HttpUrl,
    /// The secret file holding the key; the key itself is never in this file.
    pub key_file: PathBuf,
    /// Sent with every chat and Decisions request. Required, so no request goes out without it.
    pub provider: ProviderSettings,
}

/// OpenRouter's routing preferences: only endpoints that don't train on or retain the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    pub data_collection: DataCollection,
    /// Zero data retention.
    pub zdr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCollection {
    Deny,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCompatibleBackend {
    pub chat_url: HttpUrl,
    /// `null` when the server wants no key.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
}

/// The core's model roles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoles {
    pub judge: JudgeRole,
    /// The chat judge that answers while Jev can't; without it the chain drops to the rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_fallback: Option<JudgeRole>,
    /// Fills the arguments of the fixed tool calls.
    pub planner: ChatRole,
    /// Writes the answer from the facts.
    pub composer: ChatRole,
}

impl ModelRoles {
    fn backends(&self) -> Vec<(&'static str, BackendName)> {
        let mut out = vec![("judge", self.judge.backend)];
        if let Some(fallback) = &self.judge_fallback {
            out.push(("judge_fallback", fallback.backend));
        }
        out.push(("planner", self.planner.backend));
        out.push(("composer", self.composer.backend));
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgeKind {
    /// Jev through OpenRouter's Decisions API.
    Jev,
    /// A chat model answering with JSON probabilities; uncalibrated, so thresholds tighten.
    Chat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeRole {
    pub backend: BackendName,
    pub kind: JudgeKind,
    pub model: ModelSlug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    #[serde(
        default,
        deserialize_with = "temperature",
        skip_serializing_if = "Option::is_none"
    )]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRole {
    pub backend: BackendName,
    pub model: ModelSlug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    #[serde(
        default,
        deserialize_with = "temperature",
        skip_serializing_if = "Option::is_none"
    )]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

fn temperature<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f32>, D::Error> {
    match Option::<f32>::deserialize(deserializer)? {
        Some(t) if !(0.0..=2.0).contains(&t) => {
            Err(serde::de::Error::custom("a temperature is between 0 and 2"))
        }
        t => Ok(t),
    }
}

/// Spending caps. Over a cap the question fails with `budget_exceeded`, never a silent downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub per_question_usd: Usd,
    pub per_user_daily_usd: Usd,
    pub global_daily_usd: Usd,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = r#"
backends:
  openrouter:
    chat_url: https://openrouter.ai/api/v1/chat/completions
    decisions_url: https://openrouter.ai/api/alpha/decisions
    key_file: /run/secrets/openrouter_api_key
    provider:
      data_collection: deny
      zdr: true
  openai_compatible:
    chat_url: http://llm:8000/v1/chat/completions
    key_file: null
roles:
  judge:          { backend: openrouter, kind: jev,  model: typesafe/jev-1.13, timeout_ms: 2000 }
  judge_fallback: { backend: openrouter, kind: chat, model: placeholder/small-fast-model, temperature: 0, max_tokens: 300 }
  planner:        { backend: openrouter, model: placeholder/small-fast-model, temperature: 0, max_tokens: 150 }
  composer:       { backend: openrouter, model: placeholder/strong-model, temperature: 0.2, max_tokens: 400 }
budgets:
  per_question_usd: 0.05
  per_user_daily_usd: 1.00
  global_daily_usd: 5.00
"#;

    fn parse(yaml: &str) -> Result<ModelsConfig, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    #[test]
    fn the_documented_models_parse() {
        let m = parse(MODELS).unwrap();
        let openrouter = m.backends.openrouter.as_ref().unwrap();
        assert_eq!(openrouter.provider.data_collection, DataCollection::Deny);
        assert!(openrouter.provider.zdr);
        assert_eq!(m.backends.openai_compatible.as_ref().unwrap().key_file, None);
        assert_eq!(m.roles.judge.kind, JudgeKind::Jev);
        assert_eq!(m.roles.judge.model.as_str(), "typesafe/jev-1.13");
        assert_eq!(m.roles.composer.temperature, Some(0.2));
        assert_eq!(m.budgets.global_daily_usd.get(), 5.0);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse(&MODELS.replace("zdr: true", "zdr: true\n      sort: price")).is_err());
        assert!(parse(&MODELS.replace("max_tokens: 150", "max_tokens: 150, kind: chat")).is_err());
        assert!(parse(&MODELS.replace("  composer:", "  summarizer:")).is_err());
        assert!(parse(&MODELS.replace("  openai_compatible:\n", "  ollama:\n")).is_err());
    }

    #[test]
    fn provider_settings_are_required_on_openrouter() {
        let without = MODELS.replace(
            "    provider:\n      data_collection: deny\n      zdr: true\n",
            "",
        );
        assert!(parse(&without).is_err());
    }

    #[test]
    fn jev_runs_only_on_openrouter() {
        let local = MODELS.replace(
            "backend: openrouter, kind: jev",
            "backend: openai_compatible, kind: jev",
        );
        let err = parse(&local).unwrap_err().to_string();
        assert!(err.contains("openrouter backend"), "{err}");
        assert!(
            parse(&MODELS.replace(
                "backend: openrouter, kind: jev",
                "backend: openai_compatible, kind: chat"
            ))
            .is_ok()
        );
    }

    #[test]
    fn the_fallback_is_a_chat_judge() {
        assert!(parse(&MODELS.replace("kind: chat, model", "kind: jev, model")).is_err());
        let without = MODELS.replace(
            "  judge_fallback: { backend: openrouter, kind: chat, model: placeholder/small-fast-model, temperature: 0, max_tokens: 300 }\n",
            "",
        );
        assert!(parse(&without).unwrap().roles.judge_fallback.is_none());
    }

    #[test]
    fn a_role_names_a_configured_backend() {
        let only_local = MODELS.replace(
            "  openai_compatible:\n    chat_url: http://llm:8000/v1/chat/completions\n    key_file: null\n",
            "",
        );
        let err = parse(&only_local.replace(
            "planner:        { backend: openrouter",
            "planner:        { backend: openai_compatible",
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("planner"), "{err}");
    }

    #[test]
    fn values_are_in_range() {
        assert!(parse(&MODELS.replace("temperature: 0.2", "temperature: 3")).is_err());
        assert!(parse(&MODELS.replace("global_daily_usd: 5.00", "global_daily_usd: -1")).is_err());
        assert!(parse(&MODELS.replace("model: typesafe/jev-1.13", "model: \"jev 1.13\"")).is_err());
    }
}
