//! The orchestrator's parts from the configuration: the judge chain from `models.yaml` (Jev, then
//! the chat judge; without Jev, as in the `strict` preset, the chat judge alone), the planner and
//! the composer as chat clients on their roles' backends, and each one's newest prompt from
//! `<config>/prompts/`. A key file that isn't there leaves its model to fail fast: the chain falls
//! to the next judge and in the end to the rules, the planner to the defaults, the composer to
//! the template.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mercury_judge::{
    ApiKey, ChatClient, ChatConfig, ChatJudge, DEFAULT_CHAT_TIMEOUT, DEFAULT_TIMEOUT, JevConfig, JevJudge,
    Judge, Prompt, ResilientJudge, RulesJudge, ZERO_RETENTION,
};
use mercury_schema::config::{BackendName, ChatRole, JudgeKind, JudgeRole, ModelsConfig, ProviderSettings};

use super::{BuildError, Model, Parts, ProbeAccess};
use crate::config::Config;
use crate::db::Db;

/// Where a role's requests go.
struct Endpoint {
    chat_url: String,
    decisions_url: Option<String>,
    key: Option<ApiKey>,
    /// Whether a request without a key is an error: always on openrouter, and on a model server
    /// only when its key file is configured (then a missing or empty one fails closed).
    key_required: bool,
    provider: ProviderSettings,
    https_only: bool,
}

/// Each backend's key, read once.
struct Keys {
    openrouter: Option<ApiKey>,
    openai_compatible: Option<ApiKey>,
}

impl Keys {
    fn read(models: &ModelsConfig) -> Self {
        let read = |backend: BackendName, path: &Path| match ApiKey::read(path) {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!(backend = backend.as_str(), error = %err.kind(), "a backend's key file can't be read; its models fail until it is there");
                None
            }
        };
        Self {
            openrouter: models
                .backends
                .openrouter
                .as_ref()
                .and_then(|b| read(BackendName::Openrouter, &b.key_file)),
            openai_compatible: models
                .backends
                .openai_compatible
                .as_ref()
                .and_then(|b| b.key_file.as_deref())
                .and_then(|path| read(BackendName::OpenaiCompatible, path)),
        }
    }
}

fn endpoint(models: &ModelsConfig, keys: &Keys, backend: BackendName) -> Result<Endpoint, BuildError> {
    let missing = || BuildError::Model(format!("models.yaml has no `{}` backend", backend.as_str()));
    match backend {
        BackendName::Openrouter => {
            let b = models.backends.openrouter.as_ref().ok_or_else(missing)?;
            Ok(Endpoint {
                chat_url: b.chat_url.to_string(),
                decisions_url: Some(b.decisions_url.to_string()),
                key: keys.openrouter.clone(),
                key_required: true,
                provider: b.provider,
                https_only: true,
            })
        }
        BackendName::OpenaiCompatible => {
            let b = models.backends.openai_compatible.as_ref().ok_or_else(missing)?;
            Ok(Endpoint {
                chat_url: b.chat_url.to_string(),
                decisions_url: None,
                key: keys.openai_compatible.clone(),
                // `key_file: null` is a server that wants no key; a file that can't be read is not
                key_required: b.key_file.is_some(),
                provider: ZERO_RETENTION,
                // a model server on your organisation's own network
                https_only: false,
            })
        }
    }
}

fn millis(ms: Option<u32>, default: Duration) -> Duration {
    ms.map_or(default, |ms| Duration::from_millis(u64::from(ms)))
}

fn chat_client(
    models: &ModelsConfig,
    keys: &Keys,
    backend: BackendName,
    model: &str,
    timeout_ms: Option<u32>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<ChatClient, BuildError> {
    let at = endpoint(models, keys, backend)?;
    ChatClient::new(ChatConfig {
        url: at.chat_url,
        model: model.to_owned(),
        key: at.key,
        key_required: at.key_required,
        timeout: millis(timeout_ms, DEFAULT_CHAT_TIMEOUT),
        temperature,
        max_tokens,
        provider: at.provider,
        https_only: at.https_only,
    })
    .map_err(|err| BuildError::Model(format!("a chat client can't be built: {err}")))
}

fn chat_judge(
    models: &ModelsConfig,
    keys: &Keys,
    role: &JudgeRole,
    prompts: &Path,
) -> Result<Box<dyn Judge>, BuildError> {
    let client = chat_client(
        models,
        keys,
        role.backend,
        role.model.as_str(),
        role.timeout_ms,
        role.temperature,
        role.max_tokens,
    )?;
    Ok(Box::new(ChatJudge::new(
        client,
        newest(prompts, "judge_fallback")?,
    )))
}

/// A judge of the chain, or none in that place.
type Slot = Option<Box<dyn Judge>>;

/// Jev, then the chat judge. The last judge of the chain answers nothing: when it is reached, the
/// orchestrator asks the stage's own rules, which only it can build.
fn judge_chain(models: &ModelsConfig, keys: &Keys, prompts: &Path) -> Result<Arc<dyn Judge>, BuildError> {
    let judge = &models.roles.judge;
    let (jev, chat): (Slot, Slot) = match judge.kind {
        JudgeKind::Jev => {
            let at = endpoint(models, keys, judge.backend)?;
            let url = at
                .decisions_url
                .ok_or_else(|| BuildError::Model("Jev runs only on the openrouter backend".to_owned()))?;
            let jev = JevJudge::new(JevConfig {
                url,
                model: judge.model.as_str().to_owned(),
                key: at.key,
                timeout: millis(judge.timeout_ms, DEFAULT_TIMEOUT),
                provider: at.provider,
                https_only: at.https_only,
            })
            .map_err(|err| BuildError::Model(format!("the Jev client can't be built: {err}")))?;
            let fallback = match &models.roles.judge_fallback {
                Some(role) => Some(chat_judge(models, keys, role, prompts)?),
                None => None,
            };
            (Some(Box::new(jev)), fallback)
        }
        JudgeKind::Chat => (None, Some(chat_judge(models, keys, judge, prompts)?)),
    };
    let nothing = Box::new(RulesJudge::new(HashMap::new()));
    Ok(Arc::new(ResilientJudge::new(jev, chat, nothing)))
}

fn writer(
    models: &ModelsConfig,
    keys: &Keys,
    role: &ChatRole,
    prompts: &Path,
    name: &str,
) -> Result<Model, BuildError> {
    let client = chat_client(
        models,
        keys,
        role.backend,
        role.model.as_str(),
        role.timeout_ms,
        role.temperature,
        role.max_tokens,
    )?;
    Ok(Model {
        writer: Arc::new(client),
        prompt: newest(prompts, name)?,
    })
}

/// The newest version of a role's prompt: `<role>.v<N>.md` with the highest `N`.
pub fn newest(prompts: &Path, role: &str) -> Result<Prompt, BuildError> {
    let unreadable = |_| BuildError::Model(format!("the prompts in {} can't be read", prompts.display()));
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in std::fs::read_dir(prompts).map_err(unreadable)? {
        let path = entry.map_err(unreadable)?.path();
        let version = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".md"))
            .and_then(|name| name.strip_prefix(role))
            .and_then(|rest| rest.strip_prefix(".v"))
            .and_then(|n| n.parse::<u32>().ok());
        if let Some(n) = version
            && best.as_ref().is_none_or(|(b, _)| n > *b)
        {
            best = Some((n, path));
        }
    }
    let (_, path) =
        best.ok_or_else(|| BuildError::Model(format!("no {role} prompt in {}", prompts.display())))?;
    Prompt::read(&path)
        .map_err(|err| BuildError::Model(format!("the {role} prompt can't be read: {}", err.kind())))
}

/// Everything the orchestrator needs, from the loaded configuration and its directory.
pub fn parts(
    db: Db,
    config: Arc<Config>,
    config_dir: &Path,
    probes: Arc<dyn ProbeAccess>,
) -> Result<Parts, BuildError> {
    let prompts = config_dir.join("prompts");
    let models = &config.models;
    let keys = Keys::read(models);
    Ok(Parts {
        judge: judge_chain(models, &keys, &prompts)?,
        planner: Some(writer(models, &keys, &models.roles.planner, &prompts, "planner")?),
        composer: Some(writer(
            models,
            &keys,
            &models.roles.composer,
            &prompts,
            "composer",
        )?),
        db,
        config: config.clone(),
        probes,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::orchestrator::compose;

    fn examples() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples/prompts")
    }

    /// The newest composer prompt is v3, with the two placeholders the core fills and its last two
    /// lines as the replay server of the runs on Compose reads them. v1 and v2 stay as the record of
    /// what answered older questions; the planner's prompt is as it was.
    #[test]
    fn the_newest_composer_is_v3_and_keeps_its_placeholders() {
        let dir = examples();
        let composer = newest(&dir, "composer").unwrap();
        assert_eq!(composer.version, "composer.v3");
        let lines: Vec<&str> = composer.text.trim_end().lines().collect();
        assert!(lines[lines.len() - 2].starts_with("FACTS: {facts_json}"));
        assert!(lines[lines.len() - 1].starts_with("QUESTION: {question}"));
        for old in ["composer.v1.md", "composer.v2.md"] {
            assert!(dir.join(old).is_file(), "{old} stays");
        }
        assert_eq!(newest(&dir, "planner").unwrap().version, "planner.v1");

        // filled in, a finding reads as the code reader's, and the question comes last
        let finding = json!([{ "id": "f_c5r0", "source": "code_reader", "confidence": 0.82,
                               "text": "The export writes one file per invoice batch" }]);
        let filled = compose::prompt(
            &composer.text,
            &finding,
            "How long would an export to Excel take?",
        );
        assert!(filled.contains(r#""source":"code_reader""#));
        assert!(filled.contains("\nFACTS: [{"));
        assert!(
            filled
                .trim_end()
                .ends_with("QUESTION: How long would an export to Excel take?")
        );
        assert!(!filled.contains("{facts_json}") && !filled.contains("{question}"));
    }

    /// The estimate's wording, which the estimate-versus-promise sentences of the judge and the
    /// scenarios on Compose use, is in the prompt as written.
    #[test]
    fn the_composer_words_an_estimate_as_an_estimate() {
        let composer = newest(&examples(), "composer").unwrap();
        assert!(
            composer
                .text
                .contains("an estimate from the repository, not a promise")
        );
        assert!(
            composer
                .text
                .contains("Never give a client a date, a price or a commitment.")
        );
    }
}
