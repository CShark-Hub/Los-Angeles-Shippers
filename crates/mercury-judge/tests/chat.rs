//! The chat client and the chat judge against a stand-in for OpenRouter's chat endpoint. Every
//! way the endpoint or the model can misbehave ends in an error, never in an outcome; the valid
//! replies come last.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use indexmap::IndexMap;
use mercury_judge::{
    Answer, ApiKey, Batch, ChatClient, ChatConfig, ChatError, ChatJudge, DEFAULT_CHAT_TIMEOUT, Format, Judge,
    JudgeError, Message, Outcome, Prompt, Question,
};
use mercury_schema::JudgeMode;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT: &str = "/api/v1/chat/completions";
const MODEL: &str = "openai/gpt-4.1-mini";

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

/// The fallback judge's prompt as the core ships it.
fn prompt() -> Prompt {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples/prompts/judge_fallback.v1.md");
    Prompt::read(&file).unwrap()
}

/// A config for the stand-in, which speaks plain HTTP.
fn config(server: &MockServer) -> ChatConfig {
    let mut config = ChatConfig::openrouter(
        format!("{}{CHAT}", server.uri()),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    config.https_only = false;
    config.temperature = Some(0.0);
    config.max_tokens = Some(300);
    config
}

/// Valid answers to every question of [`batch`], as the model writes them.
fn answers() -> Value {
    json!({
        "asks_for_code": {"type": "noul", "noul": 0.04},
        "class": {"type": "choice", "choice": "tested",
                  "probabilities": {"progress": 0.15, "tested": 0.8, "forbidden": 0.05}, "confidence": 0.7},
        "health": {"type": "score", "score": 1.6, "probabilities": {"0": 0.1, "1": 0.2, "2": 0.7},
                   "confidence": 0.6}
    })
}

fn answered(answers: Value) -> String {
    json!({ "answers": answers }).to_string()
}

/// A chat reply whose answer is `content`.
fn reply(content: Value) -> Value {
    json!({
        "id": "gen-chat-1", "model": "openai/gpt-4.1-mini-2025-04-14", "object": "chat.completion",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 640, "completion_tokens": 96, "total_tokens": 736, "cost": 0.00041}
    })
}

/// A valid reply with the answer to `id` replaced.
fn reply_with(id: &str, answer: Value) -> Value {
    let mut all = answers();
    all[id] = answer;
    reply(json!(answered(all)))
}

/// Judges [`batch`] against an endpoint that gives `response` to every request.
async fn judge_against(response: ResponseTemplate) -> Result<Outcome, JudgeError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT))
        .respond_with(response)
        .mount(&server)
        .await;
    let client = ChatClient::new(config(&server)).unwrap();
    ChatJudge::new(client, prompt()).decide(&batch()).await
}

async fn judge_reply(body: Value) -> Result<Outcome, JudgeError> {
    judge_against(ResponseTemplate::new(200).set_body_json(body)).await
}

fn assert_malformed(result: Result<Outcome, JudgeError>, case: &str) {
    assert!(
        matches!(result, Err(JudgeError::Malformed(_))),
        "{case}: expected a malformed response, got {result:?}"
    );
}

fn assert_invalid(result: Result<Outcome, JudgeError>, question: &str, case: &str) {
    match result {
        Err(JudgeError::InvalidAnswer { id, .. }) => assert_eq!(id, question, "{case}"),
        other => panic!("{case}: expected an invalid answer to `{question}`, got {other:?}"),
    }
}

#[tokio::test]
async fn every_status_but_success_is_an_error_even_with_valid_answers() {
    for status in [
        301, 400, 401, 402, 403, 404, 408, 413, 429, 500, 502, 503, 524, 529,
    ] {
        let response = ResponseTemplate::new(status).set_body_json(reply(json!(answered(answers()))));
        assert_eq!(judge_against(response).await, Err(JudgeError::Status(status)));
    }
}

#[tokio::test]
async fn a_slow_answer_is_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(reply(json!(answered(answers()))))
                .set_delay(Duration::from_millis(1500)),
        )
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.timeout = Duration::from_millis(200);
    let judge = ChatJudge::new(ChatClient::new(config).unwrap(), prompt());
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::Timeout(200)));
}

#[test]
fn the_default_timeout_is_ten_seconds() {
    assert_eq!(DEFAULT_CHAT_TIMEOUT, Duration::from_secs(10));
    let config = ChatConfig::openrouter("https://openrouter.ai/api/v1/chat/completions", MODEL, None);
    assert_eq!(config.timeout, DEFAULT_CHAT_TIMEOUT);
}

#[tokio::test]
async fn without_a_key_nothing_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(json!(answered(answers())))))
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.key = None;
    let client = ChatClient::new(config).unwrap();
    let messages = [Message::user("What is blocked?")];
    assert_eq!(
        client.complete(&messages, Format::Text).await,
        Err(ChatError::NoKey)
    );
    let judge = ChatJudge::new(client, prompt());
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::NoKey));
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn an_address_that_isnt_https_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(json!(answered(answers())))))
        .mount(&server)
        .await;
    let config = ChatConfig::openrouter(
        format!("{}{CHAT}", server.uri()),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    assert!(config.https_only, "HTTPS is the default");
    assert_eq!(ChatClient::new(config).unwrap_err(), ChatError::InsecureUrl);
    assert_eq!(JudgeError::from(ChatError::InsecureUrl), JudgeError::InsecureUrl);
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
    let mut config = ChatConfig::openrouter(
        format!("http://127.0.0.1:{port}{CHAT}"),
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    config.https_only = false;
    // long enough for a refused connection to be reported as one, also on Windows
    config.timeout = Duration::from_secs(10);
    let judge = ChatJudge::new(ChatClient::new(config).unwrap(), prompt());
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::Transport));
}

#[tokio::test]
async fn a_batch_no_judge_may_answer_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(json!(answered(answers())))))
        .mount(&server)
        .await;
    let judge = ChatJudge::new(ChatClient::new(config(&server)).unwrap(), prompt());
    let mut no_questions = batch();
    no_questions.questions.clear();
    assert!(matches!(
        judge.decide(&no_questions).await,
        Err(JudgeError::InvalidBatch(_))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_body_that_is_not_a_chat_response_is_malformed() {
    for body in ["", "not json", "<html>bad gateway</html>", "[]", "{}"] {
        let result = judge_against(ResponseTemplate::new(200).set_body_string(body)).await;
        assert_malformed(result, &format!("{body:?}"));
    }
    let mut negative = reply(json!(answered(answers())));
    negative["usage"]["cost"] = json!(-0.1);
    let mut words = reply(json!(answered(answers())));
    words["usage"]["cost"] = json!("free");
    for (body, case) in [
        (json!({"choices": "none"}), "choices that aren't a list"),
        (negative, "a negative cost"),
        (words, "a cost in words"),
    ] {
        assert_malformed(judge_reply(body).await, case);
    }
}

#[tokio::test]
async fn a_reply_without_an_answer_or_with_an_unfinished_one_is_an_error() {
    let mut no_choice = reply(json!(answered(answers())));
    no_choice["choices"] = json!([]);
    let mut cases = vec![
        (no_choice, "no choice"),
        (reply(Value::Null), "no content"),
        (reply(json!("  \n")), "blank content"),
    ];
    for finish in ["length", "content_filter", "tool_calls", "error"] {
        let mut unfinished = reply(json!(answered(answers())));
        unfinished["choices"][0]["finish_reason"] = json!(finish);
        cases.push((unfinished, finish));
    }
    for (body, case) in cases {
        assert_malformed(judge_reply(body).await, case);
    }
}

#[tokio::test]
async fn content_that_is_not_the_answers_object_is_malformed() {
    let fenced = format!("```json\n{}\n```", answered(answers()));
    let with_reasons = json!({"answers": answers(), "reasoning": "the export has tests"}).to_string();
    for (content, case) in [
        ("I think the export is tested.".to_owned(), "prose"),
        (fenced, "JSON in a code fence"),
        ("[]".to_owned(), "a list"),
        ("{}".to_owned(), "no answers"),
        (
            json!({"answers": []}).to_string(),
            "answers that aren't an object",
        ),
        (with_reasons, "a key beside the answers"),
        (answers().to_string(), "the answers without their key"),
    ] {
        assert_malformed(judge_reply(reply(json!(content))).await, case);
    }
}

#[tokio::test]
async fn a_missing_answer_or_one_not_asked_is_an_error() {
    let mut missing = answers();
    missing.as_object_mut().unwrap().remove("class");
    assert_eq!(
        judge_reply(reply(json!(answered(missing)))).await,
        Err(JudgeError::MissingAnswer("class".to_owned()))
    );
    let mut extra = answers();
    extra["asks_to_evaluate_people"] = json!({"type": "noul", "noul": 0.1});
    assert_eq!(
        judge_reply(reply(json!(answered(extra)))).await,
        Err(JudgeError::UnexpectedAnswer)
    );
}

#[tokio::test]
async fn an_answer_that_does_not_fit_its_question_is_invalid() {
    let cases = [
        (
            "asks_for_code",
            json!({"type": "noul", "noul": 1.2}),
            "noul above 1",
        ),
        (
            "asks_for_code",
            json!({"type": "noul", "noul": "0.9"}),
            "a probability as a string",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "progress",
                   "probabilities": {"progress": 0.15, "tested": 0.8, "forbidden": 0.05}, "confidence": 0.7}),
            "a choice that is not the most probable option",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested",
                   "probabilities": {"progress": 0.2, "tested": 0.5, "forbidden": 0.1}, "confidence": 0.7}),
            "probabilities that don't sum to 1",
        ),
        (
            "class",
            json!({"type": "choice", "choice": "tested", "probabilities": {"tested": 1.0}, "confidence": 0.7}),
            "options without a probability",
        ),
        (
            "health",
            json!({"type": "score", "score": 3.0, "probabilities": {"0": 0.1, "1": 0.2, "2": 0.7}, "confidence": 0.6}),
            "a score above the top level",
        ),
        (
            "health",
            json!({"type": "noul", "noul": 0.5}),
            "a noul for a score",
        ),
    ];
    for (id, answer, case) in cases {
        assert_invalid(judge_reply(reply_with(id, answer)).await, id, case);
    }
}

/// The reply OpenRouter gave the chat model for [`batch`], recorded by `fixtures/record.mjs`.
fn recorded_reply() -> Value {
    serde_json::from_str(include_str!("fixtures/recorded/chat.json")).unwrap()
}

/// The answers inside the recorded reply: the model's own JSON, in the text of its message.
fn recorded_answers() -> Value {
    let reply = recorded_reply();
    let content = reply["choices"][0]["message"]["content"].as_str().unwrap();
    serde_json::from_str::<Value>(content).unwrap()["answers"].clone()
}

#[tokio::test]
async fn every_request_carries_the_prompt_the_batch_json_mode_and_the_provider_settings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded_reply()))
        .expect(1)
        .mount(&server)
        .await;
    let batch = batch();
    let judge = ChatJudge::new(ChatClient::new(config(&server)).unwrap(), prompt());
    judge.decide(&batch).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer not-a-real-key"
    );
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["max_tokens"], 300);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    assert_eq!(body["provider"], json!({"data_collection": "deny", "zdr": true}));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], prompt().text);
    assert_eq!(messages[1]["role"], "user");
    let asked: Value = serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        asked,
        json!({"state": batch.state, "questions": batch.questions}),
        "the batch, as the Decisions endpoint would get it"
    );
}

#[tokio::test]
async fn a_valid_reply_is_a_chat_outcome_with_every_answer_checked() {
    let reply = recorded_reply();
    let said = recorded_answers();
    let outcome = judge_reply(reply.clone()).await.unwrap();
    assert_eq!(
        outcome.mode,
        JudgeMode::Chat,
        "uncalibrated: the verdicts tighten for it"
    );
    // what the endpoint named, as it named it: OpenRouter answers with the model it was asked for,
    // not with the snapshot behind it
    assert_eq!(outcome.model.as_deref(), reply["model"].as_str());
    assert_eq!(outcome.model.as_deref(), Some(MODEL));
    assert_eq!(outcome.request_id.as_deref(), reply["id"].as_str());
    assert_eq!(outcome.prompt_version.as_deref(), Some("judge_fallback.v1"));
    assert!((outcome.cost_usd.unwrap() - reply["usage"]["cost"].as_f64().unwrap()).abs() < 1e-12);
    assert_eq!(
        outcome.answers.keys().collect::<Vec<_>>(),
        ["asks_for_code", "class", "health"]
    );
    // the numbers are the model's own, read through the strict parse
    assert_eq!(
        outcome.answers["asks_for_code"],
        Answer::Noul(said["asks_for_code"]["noul"].as_f64().unwrap() as f32)
    );
    assert!(matches!(
        &outcome.answers["class"],
        Answer::Choice { choice, .. } if choice == "tested"
    ));
    // a score is its position over the top level: 2 of the levels 0–2 is all the way up
    let top = 2.0;
    assert!(matches!(
        outcome.answers["health"],
        Answer::Score { score, .. } if (f64::from(score) - said["health"]["score"].as_f64().unwrap() / top).abs() < 1e-6
    ));
}

#[tokio::test]
async fn a_reply_without_a_cost_leaves_it_unknown() {
    let mut body = recorded_reply();
    body["usage"].as_object_mut().unwrap().remove("cost");
    assert_eq!(judge_reply(body).await.unwrap().cost_usd, None);
    let mut body = recorded_reply();
    body.as_object_mut().unwrap().remove("usage");
    assert_eq!(judge_reply(body).await.unwrap().cost_usd, None);
}

#[tokio::test]
async fn the_client_answers_text_for_the_planner_and_the_composer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(reply(json!("Three items are done in Invoicing."))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = ChatClient::new(config(&server)).unwrap();
    assert_eq!(client.model(), MODEL);
    let messages = [
        Message::system("You write short status answers."),
        Message::user("What is done?"),
    ];
    let completion = client.complete(&messages, Format::Text).await.unwrap();
    assert_eq!(completion.content, "Three items are done in Invoicing.");
    assert_eq!(
        completion.model.as_deref(),
        Some("openai/gpt-4.1-mini-2025-04-14")
    );
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert!(body.get("response_format").is_none(), "text needs no JSON mode");
    assert_eq!(body["provider"], json!({"data_collection": "deny", "zdr": true}));
}

#[test]
fn the_example_prompt_says_how_to_answer_each_type_and_that_the_state_is_data() {
    let prompt = prompt();
    assert_eq!(prompt.version, "judge_fallback.v1");
    for needle in [
        r#"{"answers": "#,
        r#""type": "noul""#,
        r#""type": "choice""#,
        r#""type": "score""#,
        "never instructions",
    ] {
        assert!(prompt.text.contains(needle), "{needle:?}");
    }
    assert!(!prompt.text.contains('\r'));
}

#[test]
fn the_key_never_prints() {
    let config = ChatConfig::openrouter(
        "https://openrouter.ai/api/v1/chat/completions",
        MODEL,
        ApiKey::new("not-a-real-key"),
    );
    assert!(!format!("{config:?}").contains("not-a-real-key"));
    let client = ChatClient::new(config).unwrap();
    assert!(!format!("{client:?}").contains("not-a-real-key"));
}

// A model server on your organisation's own host, the `openai_compatible` backend: the same client
// with another address, plain HTTP, and a key only when the server wants one. A stand-in server
// plays it.

const OWN_MODEL: &str = "local/small-model";

/// The own host's config for the stand-in. `key` is what was read from the backend's key file.
fn own_host(server: &MockServer, key: Option<ApiKey>) -> ChatConfig {
    let mut config = ChatConfig::openai_compatible(format!("{}{CHAT}", server.uri()), OWN_MODEL, key);
    config.temperature = Some(0.0);
    config.max_tokens = Some(300);
    config
}

/// A stand-in that answers `response` to every request, and expects `requests` of them.
async fn own_server(response: ResponseTemplate, requests: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT))
        .respond_with(response)
        .expect(requests)
        .mount(&server)
        .await;
    server
}

fn text_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(reply(json!("Three items are done in Invoicing.")))
}

#[tokio::test]
async fn a_server_that_wants_no_key_is_sent_none() {
    let server = own_server(text_reply(), 1).await;
    let config = own_host(&server, None);
    assert!(
        !config.https_only,
        "a model server on your organisation's network speaks plain HTTP"
    );
    assert!(
        !config.key_required,
        "no key file is configured, so none is needed"
    );
    let client = ChatClient::new(config).unwrap();
    assert_eq!(client.model(), OWN_MODEL);

    let completion = client
        .complete(&[Message::user("What is done?")], Format::Text)
        .await
        .unwrap();
    assert_eq!(completion.content, "Three items are done in Invoicing.");

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no key, no Authorization header"
    );
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], OWN_MODEL);
    assert_eq!(body["messages"][0]["content"], "What is done?");
}

#[tokio::test]
async fn a_key_is_sent_as_a_bearer_token() {
    let server = own_server(text_reply(), 1).await;
    let client = ChatClient::new(own_host(&server, ApiKey::new("local-key"))).unwrap();
    client
        .complete(&[Message::user("What is done?")], Format::Text)
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer local-key"
    );
}

#[tokio::test]
async fn a_key_that_was_asked_for_and_is_missing_sends_nothing() {
    // `key_file` names a file, but it isn't there or is empty: the operator meant the server to
    // want a key, and a request without one would go out unauthenticated
    let server = own_server(text_reply(), 0).await;
    let mut config = own_host(&server, None);
    config.key_required = true;
    let client = ChatClient::new(config).unwrap();
    assert_eq!(
        client
            .complete(&[Message::user("What is done?")], Format::Text)
            .await,
        Err(ChatError::NoKey)
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn an_error_status_from_the_own_host_is_an_error() {
    for status in [401, 404, 429, 500, 503] {
        let server = own_server(ResponseTemplate::new(status), 1).await;
        let client = ChatClient::new(own_host(&server, None)).unwrap();
        assert_eq!(
            client
                .complete(&[Message::user("What is done?")], Format::Text)
                .await,
            Err(ChatError::Status(status)),
            "HTTP {status}"
        );
    }
}

#[tokio::test]
async fn a_server_that_reports_no_cost_costs_nothing_the_budgets_know_of() {
    let server = own_server(
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1", "model": OWN_MODEL, "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 1, "total_tokens": 13}
        })),
        1,
    )
    .await;
    let client = ChatClient::new(own_host(&server, None)).unwrap();
    let completion = client
        .complete(&[Message::user("Say ok.")], Format::Text)
        .await
        .unwrap();
    assert_eq!(completion.cost_usd, None);
    assert_eq!(completion.model.as_deref(), Some(OWN_MODEL));
}

#[tokio::test]
async fn the_chat_judge_answers_over_the_own_host_as_a_chat_judge() {
    let server = own_server(ResponseTemplate::new(200).set_body_json(recorded_reply()), 1).await;
    let client = ChatClient::new(own_host(&server, None)).unwrap();
    let outcome = ChatJudge::new(client, prompt()).decide(&batch()).await.unwrap();
    assert_eq!(
        outcome.mode,
        JudgeMode::Chat,
        "the strict preset's judge: uncalibrated, tightened"
    );
    assert_eq!(
        outcome.answers["asks_for_code"],
        Answer::Noul(recorded_answers()["asks_for_code"]["noul"].as_f64().unwrap() as f32)
    );
}

#[tokio::test]
async fn the_chat_judge_without_a_key_where_one_is_asked_for_is_a_judge_error() {
    let server = own_server(ResponseTemplate::new(200).set_body_json(recorded_reply()), 0).await;
    let mut config = own_host(&server, None);
    config.key_required = true;
    let judge = ChatJudge::new(ChatClient::new(config).unwrap(), prompt());
    assert_eq!(judge.decide(&batch()).await, Err(JudgeError::NoKey));
}

#[test]
fn openrouter_still_needs_a_key_and_https() {
    let config = ChatConfig::openrouter("https://openrouter.ai/api/v1/chat/completions", MODEL, None);
    assert!(config.key_required && config.https_only);
}

#[tokio::test]
async fn a_slow_own_host_is_a_timeout() {
    let server = own_server(text_reply().set_delay(Duration::from_millis(1500)), 1).await;
    let mut config = own_host(&server, None);
    config.timeout = Duration::from_millis(200);
    let client = ChatClient::new(config).unwrap();
    assert_eq!(
        client
            .complete(&[Message::user("What is done?")], Format::Text)
            .await,
        Err(ChatError::Timeout(200))
    );
}

#[tokio::test]
async fn a_body_from_the_own_host_that_is_not_a_chat_response_is_malformed() {
    for body in ["not json", "{}", r#"{"choices": "none"}"#] {
        let server = own_server(ResponseTemplate::new(200).set_body_string(body), 1).await;
        let client = ChatClient::new(own_host(&server, None)).unwrap();
        let result = client
            .complete(&[Message::user("What is done?")], Format::Text)
            .await;
        assert!(
            matches!(result, Err(ChatError::Malformed(_))),
            "{body:?}: {result:?}"
        );
    }
}

#[tokio::test]
async fn the_chat_judge_refuses_a_value_outside_zero_and_one_from_the_own_host() {
    let server = own_server(
        ResponseTemplate::new(200)
            .set_body_json(reply_with("asks_for_code", json!({"type": "noul", "noul": 1.2}))),
        1,
    )
    .await;
    let client = ChatClient::new(own_host(&server, None)).unwrap();
    let result = ChatJudge::new(client, prompt()).decide(&batch()).await;
    assert_invalid(result, "asks_for_code", "noul above 1 from the own host");
}
