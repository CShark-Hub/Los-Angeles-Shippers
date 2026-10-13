//! The example configurations in `config-examples/presets/`: each passes the check the core starts
//! with, builds everything the core needs to start (the prompts, the model clients), says what its
//! name promises, and keeps what it shares with the examples identical to them (`demo` adds its own
//! lines, and nothing else). The `strict` preset is also run against a stand-in for its model
//! server, to show that its judge chain asks that server, with no key, and answers as a chat judge.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals and a panic is the test failing"
)]

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use indexmap::IndexMap;
use mercury_core::config::{self, Config};
use mercury_core::db::Db;
use mercury_core::orchestrator::build;
use mercury_core::probes::{McpClient, ProbeConfig, Probes};
use mercury_judge::{Answer, Batch, Question};
use mercury_schema::config::{BackendName, HttpUrl, JudgeKind, ProbeEndpoint, ProjectConfig};
use mercury_schema::manifest::{JudgeContext, ProbeManifest};
use mercury_schema::{AccessMode, JudgeMode, ProjectId, QuestionClass, Role};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const PRESETS: [&str; 3] = ["demo", "private", "strict"];

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples")
}

fn preset(name: &str) -> PathBuf {
    examples().join("presets").join(name)
}

fn load(dir: &Path) -> Config {
    let report = config::read(dir);
    assert_eq!(report.problems, [], "{}", dir.display());
    report.config.unwrap()
}

/// A file's text with Unix line ends, so a checkout with Windows line ends compares equal.
fn text(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .replace("\r\n", "\n")
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn each_preset_passes_the_check_the_core_starts_with() {
    for name in PRESETS {
        let report = config::read(&preset(name));
        assert_eq!(report.problems, [], "{name}");
        assert!(report.config.is_some(), "{name}");
        // what a deployment mounts separately is a note, not a problem
        let keys: Vec<_> = report.notes.iter().map(|n| n.key.as_str()).collect();
        assert!(
            keys.contains(&"projects.delta.probe.token_file"),
            "{name}: {keys:?}"
        );
    }
}

#[test]
fn what_a_preset_shares_with_the_examples_is_the_examples_own() {
    // the catalogue and the prompts are changed once, in the examples, and copied into the presets
    // (`bash deploy/compose/scripts/presets.sh sync`); a threshold that differs by preset is a bug
    for name in PRESETS {
        let dir = preset(name);
        assert_eq!(
            text(&dir.join("checks.yaml")),
            text(&examples().join("checks.yaml")),
            "{name}/checks.yaml"
        );
        assert_eq!(
            names(&dir.join("prompts")),
            names(&examples().join("prompts")),
            "{name}/prompts"
        );
        for prompt in names(&examples().join("prompts")) {
            assert_eq!(
                text(&dir.join("prompts").join(&prompt)),
                text(&examples().join("prompts").join(&prompt)),
                "{name}/prompts/{prompt}"
            );
        }
    }
}

#[test]
fn demo_is_the_examples_with_the_demos_own_lines() {
    assert_eq!(
        text(&preset("demo").join("models.yaml")),
        text(&examples().join("models.yaml")),
        "demo/models.yaml"
    );

    // the examples, with a manager's health question held for sign-off and the poisoned project
    let mut expected = load(&examples());
    let pm = expected.policies.roles.get_mut(&Role::Pm).unwrap();
    assert!(pm.signoff.is_empty(), "the examples hold nothing of a manager's");
    pm.signoff.push(QuestionClass::Health);
    let delta = expected
        .projects
        .projects
        .get(&ProjectId::parse("delta").unwrap())
        .unwrap();
    let poison = ProjectConfig {
        display_name: "Ember — reports".to_owned(),
        aliases: vec!["poison".to_owned(), "ember".to_owned()],
        access_max: AccessMode::CodeModel,
        manifest: None,
        probe: ProbeEndpoint {
            url: HttpUrl::parse("http://probe-poison:7000/mcp").unwrap(),
            token_file: "/run/mercury/probe-tokens/poison.token".into(),
        },
    };
    assert_eq!(delta.probe.token_file.parent(), poison.probe.token_file.parent());
    expected
        .projects
        .projects
        .insert(ProjectId::parse("poison").unwrap(), poison);

    let demo = load(&preset("demo"));
    assert_eq!(demo.policies, expected.policies, "demo/policies.yaml");
    assert_eq!(demo.projects, expected.projects, "demo/projects.yaml");
}

#[test]
fn a_preset_holds_the_files_the_core_reads_and_a_manifest_template() {
    for name in PRESETS {
        assert_eq!(
            names(&preset(name)),
            [
                "checks.yaml",
                "models.yaml",
                "policies.yaml",
                "probe.yaml",
                "projects.yaml",
                "prompts"
            ],
            "{name}"
        );
    }
}

fn manifest(name: &str) -> ProbeManifest {
    serde_norway::from_str(&text(&preset(name).join("probe.yaml")))
        .unwrap_or_else(|err| panic!("{name}/probe.yaml: {err}"))
}

#[test]
fn each_manifest_template_is_a_manifest_the_probe_accepts() {
    for name in PRESETS {
        let manifest = manifest(name);
        assert_eq!(manifest.project.as_str(), "your-project", "{name}");
        assert!(!manifest.allow.is_empty(), "{name}");
    }
}

#[test]
fn the_presets_say_what_their_names_promise() {
    let demo = load(&preset("demo"));
    let private = load(&preset("private"));
    let strict = load(&preset("strict"));

    // demo and strict show titles, and the judge may read titles and labels
    for (name, config) in [("demo", &demo), ("strict", &strict)] {
        assert!(config.policies.defaults.show_titles, "{name}");
        assert_eq!(
            manifest(name).judge_context,
            [JudgeContext::Titles, JudgeContext::Labels],
            "{name}"
        );
    }
    // private shows no title to any model and the judge reads no repository text
    assert!(!private.policies.defaults.show_titles);
    assert_eq!(manifest("private").judge_context, []);

    // demo and private: Jev and chat models on OpenRouter, nothing on a server of one's own
    for (name, config) in [("demo", &demo), ("private", &private)] {
        let roles = &config.models.roles;
        assert_eq!(roles.judge.kind, JudgeKind::Jev, "{name}");
        assert!(roles.judge_fallback.is_some(), "{name}");
        for backend in [roles.judge.backend, roles.planner.backend, roles.composer.backend] {
            assert_eq!(backend, BackendName::Openrouter, "{name}");
        }
        assert!(
            config.models.backends.openai_compatible.is_some(),
            "{name}: the examples configure it"
        );
    }

    // strict: every role on your organisation's own server, a chat judge, no Jev and no OpenRouter
    let roles = &strict.models.roles;
    assert_eq!(roles.judge.kind, JudgeKind::Chat);
    assert!(
        roles.judge_fallback.is_none(),
        "the chain is the chat judge, then the rules"
    );
    for backend in [roles.judge.backend, roles.planner.backend, roles.composer.backend] {
        assert_eq!(backend, BackendName::OpenaiCompatible);
    }
    assert!(
        strict.models.backends.openrouter.is_none(),
        "nothing leaves the network"
    );
    let own = strict.models.backends.openai_compatible.as_ref().unwrap();
    assert!(own.key_file.is_none(), "a key only if the server wants one");

    // a manager and a tech lead may ask open questions in every preset, a board member never, and the
    // repository's owner opts in: no manifest template lists `open` by itself
    for (name, config) in [("demo", &demo), ("private", &private), ("strict", &strict)] {
        for role in [Role::Pm, Role::TechLead] {
            let classes = &config.policies.roles[&role].classes;
            assert!(classes.contains(&QuestionClass::Open), "{name} {role:?}");
        }
        let board = &config.policies.roles[&Role::Board].classes;
        assert!(!board.contains(&QuestionClass::Open), "{name} board");
        assert!(
            !manifest(name).allow.contains(&QuestionClass::Open),
            "{name} template"
        );
    }
    // a question that reads the code costs more: 0.15 in the demo (the examples'), 0.05 in the others
    for (name, config, cap) in [
        ("demo", &demo, 0.15),
        ("private", &private, 0.05),
        ("strict", &strict, 0.05),
    ] {
        let per_question = config.models.budgets.per_question_usd.get();
        assert!((per_question - cap).abs() < 1e-9, "{name}: {per_question}");
    }
    // a code reader may be attached to the poisoned project, and to no other of the demo's
    let projects = &demo.projects.projects;
    assert_eq!(
        projects[&ProjectId::parse("poison").unwrap()].access_max,
        AccessMode::CodeModel
    );
    assert_eq!(
        projects[&ProjectId::parse("delta").unwrap()].access_max,
        AccessMode::Code
    );
}

/// A copy of a preset's files in a fresh directory.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

async fn db(dir: &Path) -> Db {
    Db::open(&dir.join("mercury.db")).await.unwrap()
}

fn probes_of(config: &Config, db: &Db) -> Probes {
    let (configs, _) = ProbeConfig::from_projects(&config.projects);
    Probes::new(db.clone(), McpClient::http().unwrap(), configs)
}

#[tokio::test]
async fn each_preset_builds_everything_the_core_needs_to_start() {
    for name in PRESETS {
        let work = tempfile::tempdir().unwrap();
        let dir = preset(name);
        let (config, _) = config::load(&dir).unwrap();
        let db = db(work.path()).await;
        let probes = probes_of(&config, &db);
        // the judge chain, the planner and the composer, each with its newest prompt
        if let Err(err) = build::parts(db, Arc::new(config), &dir, Arc::new(probes)) {
            panic!("{name}: {err}");
        }
    }
}

/// What the stand-in model server saw.
#[derive(Default)]
struct Seen {
    authorization: Vec<bool>,
    bodies: Vec<Value>,
}

async fn chat(State(seen): State<Arc<Mutex<Seen>>>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let mut seen = seen.lock().unwrap();
    seen.authorization.push(headers.contains_key("authorization"));
    seen.bodies.push(serde_json::from_str(&body).unwrap());
    let answers = json!({ "answers": { "asks_for_code": { "type": "noul", "noul": 0.04 } } });
    (
        StatusCode::OK,
        axum::Json(json!({
            "id": "chatcmpl-1", "model": "local/judge-model", "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": answers.to_string()},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
        })),
    )
}

#[tokio::test]
async fn the_strict_preset_asks_its_own_server_with_no_key_and_judges_in_chat_mode() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(seen.clone());
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // the preset as shipped, with its model server's address pointed at the stand-in
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("config");
    copy_dir(&preset("strict"), &dir);
    let models = text(&dir.join("models.yaml")).replace("http://llm:8000", &format!("http://{addr}"));
    fs::write(dir.join("models.yaml"), models).unwrap();

    let (config, _) = config::load(&dir).unwrap();
    let db = db(work.path()).await;
    let probes = probes_of(&config, &db);
    let parts = build::parts(db, Arc::new(config), &dir, Arc::new(probes)).unwrap();

    let batch = Batch {
        state: json!({"question": {"text": "What is blocked in Export?", "role": "pm"}}),
        questions: IndexMap::from([(
            "asks_for_code".to_owned(),
            Question::Noul {
                instructions: json!("`question.text` asks to see source code or file paths."),
                criteria: None,
            },
        )]),
    };
    let outcome = parts.judge.decide(&batch).await.unwrap();

    assert_eq!(
        outcome.mode,
        JudgeMode::Chat,
        "the strict chain: the chat judge on its own server"
    );
    assert_eq!(outcome.answers["asks_for_code"], Answer::Noul(0.04));
    assert_eq!(outcome.model.as_deref(), Some("local/judge-model"));
    assert_eq!(outcome.cost_usd, None, "a local server reports no cost");
    assert_eq!(outcome.prompt_version.as_deref(), Some("judge_fallback.v2"));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.authorization, [false], "key_file: null sends no key");
    assert_eq!(seen.bodies.len(), 1);
    assert_eq!(seen.bodies[0]["model"], "local/judge-model");
}
