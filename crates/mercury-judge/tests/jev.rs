//! The Jev client against a stand-in for the Decisions endpoint. Every way the endpoint can
//! misbehave ends in an error, never in an outcome; the valid replies come last.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::net::TcpListener;
use std::time::Duration;

use indexmap::IndexMap;
use mercury_judge::{
    Answer, ApiKey, Batch, DEFAULT_TIMEOUT, JevConfig, JevJudge, Judge, JudgeError, Outcome, Question,
};
use mercury_schema::JudgeMode;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DECISIONS: &str = "/api/alpha/decisions";
const MODEL: &str = "typesafe/jev-1.13";

/// One question of each type, about one state.
fn batch() -> Batch {
    let classes = [
        ("progress", Value::Null),
        ("tested", Value::Null),
        ("forbidden", json!("asks for something Mercury never shows")),
    ];
    Batch {
        state: json!({"question": {"text": "Is the export tested?", "role": "pm", "project": "delta"}}),
        questions: IndexMap::from([
            (
                "asks_for_code".to_owned(),
                Question::Noul {
                    instructions: json!("`question.text` asks to see source code or file paths."),
                    criteria: None,
                },
            ),
            (
                "class".to_owned(),
                Question::Choice {
                    instructions: json!("The kind of question `question.text` is."),
                    criteria: classes.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
                },
            ),
            (
                "health".to_owned(),
                Question::Score {
                    instructions: json!("How healthy the build of `question.project` is."),
                    criteria: vec![json!("failing"), json!("flaky"), json!("green")],
                },
            ),
        ]),
    }
}

/// A valid answer to each question of [`batch`].
fn answers() -> Value {
    json!({
        "asks_for_code": {"type": "noul", "noul": 0.03},
        "class": {"type": "choice", "choice": "tested",
                  "probabilities": {"progress": 0.1, "tested": 0.85, "forbidden": 0.05}, "confidence": 0.8},
        "health": {"type": "score", "score": 1.5, "probabilities": {"0": 0.1, "1": 0.3, "2": 0.6},
                   "legend": {"0": "failing", "1": "flaky", "2": "green"}, "confidence": 0.7}
    })
}

fn reply(answers: Value) -> Value {
    json!({
        "id": "gen-dec-1", "model": "typesafe/jev-1.13-20260917", "provider": "TypeSafe",
        "answers": answers, "usage": {"input_tokens": 212, "output_tokens": 0, "cost": 0.00001}
    })
}

/// A valid reply with the answer to `id` replaced.
fn reply_with(id: &str, answer: Value) -> Value {
    let mut all = answers();
    all[id] = answer;
    reply(all)
}

/// A config for the stand-in, which speaks plain HTTP.
fn config(server: &MockServer) -> JevConfig {
    let mut config = JevConfig::new(
        format!("{}{DECISIONS}", server.uri()),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    config.https_only = false;
    config
}

/// Decides [`batch`] against an endpoint that gives `response` to every request.
async fn decide_against(response: ResponseTemplate) -> Result<Outcome, JudgeError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(DECISIONS))
        .respond_with(response)
        .mount(&server)
        .await;
    JevJudge::new(config(&server)).unwrap().decide(&batch()).await
}

async fn decide_reply(body: Value) -> Result<Outcome, JudgeError> {
    decide_against(ResponseTemplate::new(200).set_body_json(body)).await
}

fn assert_invalid(result: Result<Outcome, JudgeError>, question: &str, case: &str) {
    match result {
        Err(JudgeError::InvalidAnswer { id, .. }) => assert_eq!(id, question, "{case}"),
        other => panic!("{case}: expected an invalid answer to `{question}`, got {other:?}"),
    }
}

#[tokio::test]
async fn every_status_but_success_is_an_error_even_with_valid_answers() {
    for status in [301, 400, 401, 402, 403, 404, 413, 429, 500, 502, 503, 524, 529] {
        let result = decide_against(ResponseTemplate::new(status).set_body_json(reply(answers()))).await;
        assert_eq!(result, Err(JudgeError::Status(status)));
    }
}

#[tokio::test]
async fn a_slow_answer_is_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(reply(answers()))
                .set_delay(Duration::from_millis(1500)),
        )
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.timeout = Duration::from_millis(200);
    let judge = JevJudge::new(config).unwrap();
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::Timeout(200)));
}

#[tokio::test]
async fn the_default_budget_is_two_seconds() {
    assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(2));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(reply(answers()))
                .set_delay(Duration::from_millis(2500)),
        )
        .mount(&server)
        .await;
    let config = config(&server);
    assert_eq!(config.timeout, DEFAULT_TIMEOUT);
    let judge = JevJudge::new(config).unwrap();
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::Timeout(2000)));
}

#[tokio::test]
async fn without_a_key_nothing_is_sent() {
    assert!(ApiKey::new("").is_none());
    assert!(ApiKey::new(" \n").is_none(), "an empty key file is no key");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(answers())))
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.key = None;
    let judge = JevJudge::new(config).unwrap();
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::NoKey));
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn an_unreachable_endpoint_is_an_error() {
    // a port that was free a moment ago; nothing listens on it now
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = JevConfig::new(
        format!("http://127.0.0.1:{port}{DECISIONS}"),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    config.https_only = false;
    // long enough for a refused connection to be reported as one, also on Windows
    config.timeout = Duration::from_secs(10);
    let judge = JevJudge::new(config).unwrap();
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::Transport));
}

#[tokio::test]
async fn an_address_that_isnt_https_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(answers())))
        .mount(&server)
        .await;
    let config = JevConfig::new(
        format!("{}{DECISIONS}", server.uri()),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    assert!(config.https_only, "HTTPS is the default");
    assert_eq!(JevJudge::new(config).unwrap_err(), JudgeError::InsecureUrl);
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_batch_no_judge_may_answer_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(answers())))
        .mount(&server)
        .await;
    let judge = JevJudge::new(config(&server)).unwrap();
    let mut one_option = batch();
    one_option.questions["class"] = Question::Choice {
        instructions: json!("The kind of question it is."),
        criteria: IndexMap::from([("progress".to_owned(), Value::Null)]),
    };
    let mut no_questions = batch();
    no_questions.questions.clear();
    for bad in [one_option, no_questions] {
        assert!(matches!(
            judge.decide(&bad).await,
            Err(JudgeError::InvalidBatch(_))
        ));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_body_that_is_not_a_decisions_response_is_malformed() {
    for body in ["", "not json", "<html>bad gateway</html>", "[]", "{}"] {
        let result = decide_against(ResponseTemplate::new(200).set_body_string(body)).await;
        assert!(
            matches!(result, Err(JudgeError::Malformed(_))),
            "{body:?}: {result:?}"
        );
    }
    let usage = json!({"input_tokens": 1, "output_tokens": 0});
    for body in [
        json!({"model": MODEL, "answers": answers()}),
        json!({"model": MODEL, "usage": usage}),
        json!({"answers": answers(), "usage": usage}),
        json!({"model": MODEL, "answers": [], "usage": usage}),
        json!({"model": MODEL, "answers": answers(), "usage": {"cost": -0.1}}),
        json!({"model": MODEL, "answers": answers(), "usage": {"cost": "free"}}),
    ] {
        let result = decide_reply(body.clone()).await;
        assert!(
            matches!(result, Err(JudgeError::Malformed(_))),
            "{body}: {result:?}"
        );
    }
}

#[tokio::test]
async fn a_missing_answer_is_an_error() {
    let mut all = answers();
    all.as_object_mut().unwrap().remove("class");
    assert_eq!(
        decide_reply(reply(all)).await,
        Err(JudgeError::MissingAnswer("class".to_owned()))
    );
    assert_eq!(
        decide_reply(reply(json!({}))).await,
        Err(JudgeError::MissingAnswer("asks_for_code".to_owned()))
    );
}

#[tokio::test]
async fn an_answer_to_a_question_not_asked_is_an_error() {
    let mut all = answers();
    all["asks_to_evaluate_people"] = json!({"type": "noul", "noul": 0.1});
    assert_eq!(decide_reply(reply(all)).await, Err(JudgeError::UnexpectedAnswer));
}

#[tokio::test]
async fn a_value_outside_zero_to_one_is_an_error() {
    let cases = [
        (
            "asks_for_code",
            json!({"type": "noul", "noul": 1.2}),
            "noul above 1",
        ),
        (
            "asks_for_code",
            json!({"type": "noul", "noul": -0.01}),
            "noul below 0",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested",
                   "probabilities": {"progress": -0.5, "tested": 1.5, "forbidden": 0.0}, "confidence": 0.8}),
            "probabilities outside 0–1 that still sum to 1",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested",
                   "probabilities": {"progress": 0.1, "tested": 0.85, "forbidden": 0.05}, "confidence": 1.1}),
            "confidence above 1",
        ),
        (
            "health",
            json!({"type": "score", "score": 1.5, "probabilities": {"0": 0.1, "1": 0.3, "2": 0.6}, "confidence": -0.2}),
            "confidence below 0",
        ),
        (
            "health",
            json!({"type": "score", "score": 2.5, "probabilities": {"0": 0.1, "1": 0.3, "2": 0.6}, "confidence": 0.7}),
            "score above the top level",
        ),
        (
            "health",
            json!({"type": "score", "score": -0.5, "probabilities": {"0": 0.1, "1": 0.3, "2": 0.6}, "confidence": 0.7}),
            "score below the lowest level",
        ),
    ];
    for (id, answer, case) in cases {
        assert_invalid(decide_reply(reply_with(id, answer)).await, id, case);
    }
}

#[tokio::test]
async fn an_answer_that_does_not_fit_its_question_is_an_error() {
    let cases = [
        (
            "asks_for_code",
            json!({"type": "noul", "noul": "0.9"}),
            "a probability as a string",
        ),
        ("asks_for_code", json!({"type": "noul"}), "no value"),
        ("asks_for_code", json!(null), "null"),
        (
            "asks_for_code",
            json!({"type": "verdict", "verdict": "pass"}),
            "an unknown type",
        ),
        (
            "asks_for_code",
            json!({"type": "choice", "choice": "yes", "probabilities": {"yes": 1.0}, "confidence": 1.0}),
            "a different type than asked",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "other",
                   "probabilities": {"progress": 0.1, "tested": 0.85, "forbidden": 0.05}, "confidence": 0.8}),
            "a choice that is not an option",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested", "probabilities": {"progress": 0.15, "tested": 0.85}, "confidence": 0.8}),
            "an option without a probability",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested",
                   "probabilities": {"progress": 0.1, "tested": 0.8, "forbidden": 0.05, "other": 0.05}, "confidence": 0.8}),
            "a probability for an option not offered",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested",
                   "probabilities": {"progress": 0.1, "tested": 0.5, "forbidden": 0.1}, "confidence": 0.8}),
            "probabilities that don't sum to 1",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "progress",
                   "probabilities": {"progress": 0.1, "tested": 0.85, "forbidden": 0.05}, "confidence": 0.8}),
            "a choice that is not the most probable option",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "progress",
                   "probabilities": {"progress": 0.49, "tested": 0.5, "forbidden": 0.01}, "confidence": 0.5}),
            "a choice 0.01 less probable than another option",
        ),
        (
            "health",
            json!({"type": "score", "score": 1.5,
                   "probabilities": {"failing": 0.1, "flaky": 0.3, "green": 0.6}, "confidence": 0.7}),
            "probabilities keyed by level description",
        ),
    ];
    for (id, answer, case) in cases {
        assert_invalid(decide_reply(reply_with(id, answer)).await, id, case);
    }
}

/// The reply the Decisions endpoint gave for [`batch`], recorded by `fixtures/record.mjs`.
fn recorded_reply() -> Value {
    serde_json::from_str(include_str!("fixtures/recorded/decisions.json")).unwrap()
}

#[tokio::test]
async fn a_valid_reply_is_an_outcome_with_every_answer_checked() {
    let server = MockServer::start().await;
    let batch = batch();
    let reply = recorded_reply();
    let said = &reply["answers"];
    Mock::given(method("POST"))
        .and(path(DECISIONS))
        .and(header("authorization", "Bearer not-a-real-key"))
        .and(body_json(json!({
            "model": MODEL,
            "provider": {"data_collection": "deny", "zdr": true},
            "state": batch.state,
            "questions": batch.questions,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let outcome = JevJudge::new(config(&server))
        .unwrap()
        .decide(&batch)
        .await
        .unwrap();
    assert_eq!(outcome.mode, JudgeMode::Jev);
    assert_eq!(outcome.request_id.as_deref(), reply["id"].as_str());
    assert_eq!(
        outcome.model.as_deref(),
        Some("typesafe/jev-1.13-20260917"),
        "the dated version that answered"
    );
    assert_eq!(outcome.prompt_version, None);
    assert!((outcome.cost_usd.unwrap() - reply["usage"]["cost"].as_f64().unwrap()).abs() < 1e-12);
    assert_eq!(
        outcome.answers.keys().collect::<Vec<_>>(),
        ["asks_for_code", "class", "health"]
    );

    // the numbers are the endpoint's own, read through the strict parse
    // the parse keeps single precision, as the endpoint gives probabilities
    let number = |value: &Value| value.as_f64().unwrap() as f32;
    let probabilities = |value: &Value| -> IndexMap<String, f32> {
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), number(v)))
            .collect()
    };
    assert_eq!(
        outcome.answers["asks_for_code"],
        Answer::Noul(number(&said["asks_for_code"]["noul"]))
    );
    assert_eq!(
        outcome.answers["class"],
        Answer::Choice {
            choice: "tested".to_owned(),
            probabilities: probabilities(&said["class"]["probabilities"]),
            confidence: number(&said["class"]["confidence"]),
        }
    );
    // a score is its position over the top level, here 2: the endpoint's 0.89 is 0.445
    assert_eq!(
        outcome.answers["health"],
        Answer::Score {
            score: number(&said["health"]["score"]) / 2.0,
            probabilities: probabilities(&said["health"]["probabilities"]),
            confidence: number(&said["health"]["confidence"]),
        }
    );
}

#[tokio::test]
async fn every_request_carries_the_zero_retention_provider_settings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"provider": {"data_collection": "deny", "zdr": true}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(answers())))
        .expect(2)
        .mount(&server)
        .await;
    // nothing set by hand: the settings are the config's default
    let judge = JevJudge::new(config(&server)).unwrap();
    for _ in 0..2 {
        assert!(judge.decide(&batch()).await.is_ok());
    }
}

#[tokio::test]
async fn a_reply_without_an_id_or_a_cost_is_valid_and_its_cost_unknown() {
    let body =
        json!({"model": MODEL, "answers": answers(), "usage": {"input_tokens": 212, "output_tokens": 0}});
    let outcome = decide_reply(body).await.unwrap();
    assert_eq!(outcome.request_id, None);
    assert_eq!(outcome.cost_usd, None, "unknown, not free");
    assert_eq!(outcome.model.as_deref(), Some(MODEL));
}

#[test]
fn the_key_never_prints() {
    let key = ApiKey::new("not-a-real-key").unwrap();
    let config = JevConfig::new("http://localhost", MODEL, Some(key));
    assert!(!format!("{config:?}").contains("not-a-real-key"));
}
