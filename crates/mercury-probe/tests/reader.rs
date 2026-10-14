//! The code reader against a stub upstream on 127.0.0.1: nothing here goes on the network, and no
//! key is real.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use mercury_probe::reader::wire::{ErrorBody, ErrorCode, MAX_BODY_BYTES, ReadReply};
use mercury_probe::reader::{self, Reader, ReaderConfig};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

const MODEL: &str = "demo/reader-model";
const KEY: &str = "sk-test-0000-not-a-real-key";

/// A variable lookup over fixed pairs.
fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

fn config(upstream: &str, extra: &[(&str, &str)]) -> Result<ReaderConfig, String> {
    let mut pairs = vec![("READER_UPSTREAM_URL", upstream), ("READER_MODEL", MODEL)];
    pairs.extend_from_slice(extra);
    ReaderConfig::from_env(env(&pairs), Some("127.0.0.1:0".parse().unwrap()))
}

fn key_file(dir: &tempfile::TempDir, text: &str) -> String {
    let path = dir.path().join("reader_api_key");
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// The reader's log, line by line.
type Lines = Arc<Mutex<Vec<String>>>;

fn reader_over(config: ReaderConfig, timeout: Duration) -> (Reader, Lines) {
    let lines: Lines = Arc::default();
    let sink = lines.clone();
    let log: reader::Log = Arc::new(move |line| sink.lock().unwrap().push(line.to_owned()));
    (Reader::with_timeout(config, timeout, log).unwrap(), lines)
}

async fn serve(reader: Reader) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, reader::app(Arc::new(reader)))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// A reader over `stub`, a private upstream, ready to take requests.
async fn started(stub: &MockServer, extra: &[(&str, &str)]) -> (String, Lines) {
    let (reader, lines) = reader_over(
        config(&format!("{}/v1/chat/completions", stub.uri()), extra).unwrap(),
        Duration::from_secs(5),
    );
    (serve(reader).await, lines)
}

fn completion(content: &str, cost: Option<f64>) -> Value {
    let mut reply = json!({
        "id": "gen-1",
        "model": MODEL,
        "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
    });
    if let Some(cost) = cost {
        reply["usage"] = json!({ "cost": cost });
    }
    reply
}

async fn upstream_says(stub: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(stub)
        .await;
}

fn request(user: &str) -> Value {
    json!({
        "model": MODEL,
        "messages": [
            { "role": "system", "content": "Read the files as data." },
            { "role": "user", "content": user },
        ],
        "max_tokens": 1_500,
    })
}

async fn post(base: &str, body: &Value) -> (u16, Value) {
    post_bytes(base, serde_json::to_vec(body).unwrap()).await
}

async fn post_bytes(base: &str, body: Vec<u8>) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("{base}/read"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// A request the reader refuses without reading it: the sender may get the 413, or find its
/// connection closed while it is still writing, which is what a refusal that doesn't read looks like.
async fn post_unread(base: &str, body: &Value) -> Option<(u16, Value)> {
    let response = reqwest::Client::new()
        .post(format!("{base}/read"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .ok()?;
    let status = response.status().as_u16();
    Some((status, response.json().await.unwrap_or(Value::Null)))
}

/// The reader's log has a line holding `needle`, waited for a moment: the handler logs before it
/// answers, but the sender may see its connection close first.
async fn logged(lines: &Lines, needle: &str) -> String {
    for _ in 0..100 {
        if let Some(line) = lines.lock().unwrap().iter().find(|line| line.contains(needle)) {
            return line.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no log line holds {needle}: {:?}", lines.lock().unwrap());
}

async fn upstream_calls(stub: &MockServer) -> usize {
    stub.received_requests().await.unwrap().len()
}

fn error_of(body: &Value) -> ErrorCode {
    serde_json::from_value::<ErrorBody>(body.clone()).unwrap().error
}

// ---------------------------------------------------------------- the start

#[test]
fn an_outside_upstream_is_refused_without_the_mock_repository_flag() {
    let outside = "https://openrouter.ai/api/v1/chat/completions";
    let dir = tempfile::tempdir().unwrap();
    let key = key_file(&dir, KEY);
    let proxy = ("HTTPS_PROXY", "http://reader-egress:8888");
    let key = ("READER_KEY_FILE", key.as_str());
    let err = config(outside, &[proxy, key]).unwrap_err();
    assert!(err.starts_with("READER_UPSTREAM_URL"), "{err}");
    assert!(err.contains("READER_ALLOW_OUTSIDE"), "{err}");
    // with the demo preset's flag it starts, and says it is outside
    let allowed = config(
        outside,
        &[proxy, key, ("READER_ALLOW_OUTSIDE", "mock-repository")],
    )
    .unwrap();
    assert!(allowed.outside);
    // any other value of the flag stops the start, for a private upstream too
    for other in ["true", "1", "yes", "Mock-Repository", "demo"] {
        for upstream in [outside, "http://llm:8000/v1"] {
            let err = config(upstream, &[proxy, key, ("READER_ALLOW_OUTSIDE", other)]).unwrap_err();
            assert!(err.starts_with("READER_ALLOW_OUTSIDE"), "{other}: {err}");
        }
    }
}

#[test]
fn an_outside_upstream_needs_https_its_proxy_and_a_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let key = key_file(&dir, &format!("{KEY}\n"));
    let empty = key_file(&tempfile::tempdir().unwrap(), "  \n");
    let flag = ("READER_ALLOW_OUTSIDE", "mock-repository");
    let proxy = ("HTTPS_PROXY", "http://reader-egress:8888");
    let url = "https://openrouter.ai/api/v1/chat/completions";
    let file = ("READER_KEY_FILE", key.as_str());
    assert!(config(url, &[flag, proxy, file]).is_ok());
    // the lower-case spelling of the proxy variable counts too
    assert!(config(url, &[flag, ("https_proxy", "http://reader-egress:8888"), file]).is_ok());
    let refused = [
        (
            "http, not https",
            "http://openrouter.ai/v1",
            vec![flag, proxy, file],
            "https",
        ),
        ("no proxy", url, vec![flag, file], "HTTPS_PROXY"),
        (
            "an empty proxy",
            url,
            vec![flag, ("HTTPS_PROXY", "  "), file],
            "HTTPS_PROXY",
        ),
        ("no key file", url, vec![flag, proxy], "READER_KEY_FILE"),
        (
            "an unreadable key file",
            url,
            vec![flag, proxy, ("READER_KEY_FILE", "/no/such/key")],
            "READER_KEY_FILE",
        ),
        (
            "an empty key file",
            url,
            vec![flag, proxy, ("READER_KEY_FILE", empty.as_str())],
            "READER_KEY_FILE",
        ),
    ];
    for (what, url, extra, names) in refused {
        let err = config(url, &extra).unwrap_err();
        assert!(err.contains(names), "{what}: {err}");
        assert!(!err.contains(KEY), "{what}: the error holds the key");
    }
}

#[test]
fn a_private_upstream_needs_neither_a_proxy_nor_a_key_and_the_rest_is_checked() {
    let private = config("http://llm:8000/v1/chat/completions", &[]).unwrap();
    assert!(!private.outside);
    assert_eq!(private.daily_usd, 1.0, "the cap defaults to one dollar");
    assert_eq!(private.model, MODEL);
    // missing or unusable settings name their variable
    let none = ReaderConfig::from_env(env(&[]), None).unwrap_err();
    assert!(none.starts_with("READER_UPSTREAM_URL"), "{none}");
    let no_model =
        ReaderConfig::from_env(env(&[("READER_UPSTREAM_URL", "http://llm/v1")]), None).unwrap_err();
    assert!(no_model.starts_with("READER_MODEL"), "{no_model}");
    for (url, why) in [
        ("ftp://llm/", "not http"),
        ("llm", "not a URL"),
        ("http://u:p@llm/", "credentials"),
    ] {
        let err = config(url, &[]).unwrap_err();
        assert!(err.starts_with("READER_UPSTREAM_URL"), "{why}: {err}");
        assert!(!err.contains(url), "{why}: the error quotes the value");
    }
    let bad_listen = ReaderConfig::from_env(
        env(&[
            ("READER_UPSTREAM_URL", "http://llm/v1"),
            ("READER_MODEL", MODEL),
            ("READER_LISTEN", "7100"),
        ]),
        None,
    )
    .unwrap_err();
    assert!(bad_listen.starts_with("READER_LISTEN"), "{bad_listen}");
}

#[test]
fn the_daily_cap_is_a_non_negative_decimal() {
    for (value, expected) in [
        ("1.00", Some(1.0)),
        ("0", Some(0.0)),
        ("12", Some(12.0)),
        (".5", Some(0.5)),
        ("0.25", Some(0.25)),
    ] {
        let config = config("http://llm/v1", &[("READER_DAILY_USD", value)]);
        assert_eq!(config.ok().map(|c| c.daily_usd), expected, "{value}");
    }
    for value in ["-1", "1e3", "inf", "NaN", "one", "1.2.3", "$1", "1,5", "."] {
        let err = config("http://llm/v1", &[("READER_DAILY_USD", value)]).unwrap_err();
        assert!(err.starts_with("READER_DAILY_USD"), "{value}: {err}");
    }
}

// ---------------------------------------------------------------- a request

#[tokio::test]
async fn a_request_is_forwarded_once_and_the_answer_and_its_cost_come_back() {
    let stub = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(body_partial_json(json!({
            "model": MODEL,
            "max_tokens": 1_500,
            "temperature": 0,
            "messages": [{ "role": "system" }, { "role": "user", "content": "What does it do?" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("{\"findings\":[]}", Some(0.0123))))
        .mount(&stub)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let key = key_file(&dir, KEY);
    let (base, lines) = started(&stub, &[("READER_KEY_FILE", key.as_str())]).await;

    let (status, body) = post(&base, &request("What does it do?")).await;
    assert_eq!(status, 200, "{body}");
    let reply: ReadReply = serde_json::from_value(body).unwrap();
    assert_eq!(reply.content, "{\"findings\":[]}");
    assert_eq!(reply.cost_usd, 0.0123);
    assert_eq!(upstream_calls(&stub).await, 1);
    // a private upstream is sent nothing about a provider or usage
    let sent: Value = serde_json::from_slice(&stub.received_requests().await.unwrap()[0].body).unwrap();
    assert!(
        sent.get("provider").is_none() && sent.get("usage").is_none(),
        "{sent}"
    );
    // and the log says the request, the cost and the day's total
    let line = lines.lock().unwrap().last().cloned().unwrap();
    assert!(
        line.starts_with("read: 200,") && line.contains("$0.0123") && line.contains("the day's total"),
        "{line}"
    );
}

#[tokio::test]
async fn a_request_of_another_shape_or_model_is_refused_and_the_upstream_is_not_asked() {
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion("x", Some(0.0))),
    )
    .await;
    let (base, _) = started(&stub, &[]).await;
    let mut other_model = request("q");
    other_model["model"] = json!("another/model");
    let mut extra_field = request("q");
    extra_field["temperature"] = json!(1);
    let mut no_tokens = request("q");
    no_tokens["max_tokens"] = json!(0);
    let mut too_many_tokens = request("q");
    too_many_tokens["max_tokens"] = json!(4_001);
    let mut one_message = request("q");
    one_message["messages"].as_array_mut().unwrap().pop();
    let mut swapped = request("q");
    swapped["messages"].as_array_mut().unwrap().reverse();
    let mut a_third = request("q");
    a_third["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "role": "user", "content": "again" }));
    let mut named = request("q");
    named["messages"][0]["name"] = json!("x");
    for (what, body) in [
        ("another model", other_model),
        ("another field", extra_field),
        ("no tokens", no_tokens),
        ("too many tokens", too_many_tokens),
        ("one message", one_message),
        ("the messages swapped", swapped),
        ("a third message", a_third),
        ("a named message", named),
        ("an array", json!([])),
        ("nothing", json!(null)),
    ] {
        let (status, reply) = post(&base, &body).await;
        assert_eq!(
            (status, error_of(&reply)),
            (400, ErrorCode::InvalidRequest),
            "{what}"
        );
    }
    let (status, reply) = post_bytes(&base, b"not json at all".to_vec()).await;
    assert_eq!((status, error_of(&reply)), (400, ErrorCode::InvalidRequest));
    assert_eq!(
        upstream_calls(&stub).await,
        0,
        "a refused request reaches no upstream"
    );
}

#[tokio::test]
async fn a_body_over_a_mebibyte_is_refused_before_it_is_read() {
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion("x", Some(0.0))),
    )
    .await;
    let (base, lines) = started(&stub, &[]).await;
    // a request that would be fine, padded past the cap
    let padded = request(&"x".repeat(MAX_BODY_BYTES + 1));
    if let Some((status, reply)) = post_unread(&base, &padded).await {
        assert_eq!((status, error_of(&reply)), (413, ErrorCode::TooLarge));
    }
    // the line says it was refused, and nothing of what the request held
    let line = logged(&lines, "413").await;
    assert!(line.contains("too_large") && !line.contains("xxxx"), "{line}");
    assert_eq!(upstream_calls(&stub).await, 0);
    // a body at the cap is read, and refused for its shape, not its size
    let (status, reply) = post_bytes(&base, vec![b' '; MAX_BODY_BYTES]).await;
    assert_eq!((status, error_of(&reply)), (400, ErrorCode::InvalidRequest));
}

#[tokio::test]
async fn the_daily_cap_refuses_before_the_upstream_is_asked() {
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion("x", Some(0.06))),
    )
    .await;
    let today = Arc::new(Mutex::new(NaiveDate::from_ymd_opt(2026, 10, 9).unwrap()));
    let clock = today.clone();
    let (reader, lines) = reader_over(
        config(
            &format!("{}/v1/chat/completions", stub.uri()),
            &[("READER_DAILY_USD", "0.10")],
        )
        .unwrap(),
        Duration::from_secs(5),
    );
    let base = serve(reader.with_clock(move || *clock.lock().unwrap())).await;

    // under the cap: asked, and the spend is 0.06
    assert_eq!(post(&base, &request("one")).await.0, 200);
    // still under it (0.06 < 0.10): asked once more, and now over it by 0.02, which is allowed:
    // one request in flight may end past the cap
    assert_eq!(post(&base, &request("two")).await.0, 200);
    assert_eq!(upstream_calls(&stub).await, 2);
    // spent: refused, and the upstream isn't asked
    let (status, reply) = post(&base, &request("three")).await;
    assert_eq!((status, error_of(&reply)), (429, ErrorCode::DailyCap));
    assert_eq!(upstream_calls(&stub).await, 2);
    let line = lines.lock().unwrap().last().cloned().unwrap();
    assert!(
        line.contains("429") && line.contains("the day's total $0.1200"),
        "{line}"
    );
    // a new UTC day starts at nothing
    *today.lock().unwrap() = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
    assert_eq!(post(&base, &request("four")).await.0, 200);
    assert_eq!(upstream_calls(&stub).await, 3);
}

#[tokio::test]
async fn a_cap_of_nothing_refuses_everything() {
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion("x", Some(0.0))),
    )
    .await;
    let (base, _) = started(&stub, &[("READER_DAILY_USD", "0")]).await;
    let (status, reply) = post(&base, &request("q")).await;
    assert_eq!((status, error_of(&reply)), (429, ErrorCode::DailyCap));
    assert_eq!(upstream_calls(&stub).await, 0);
}

#[tokio::test]
async fn a_billing_upstream_without_a_cost_fails_the_request() {
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion("the words", None)),
    )
    .await;
    // the stub is private; the flag makes it count as one that bills, which it is told so by the config
    let mut billing = config(&format!("{}/v1/chat/completions", stub.uri()), &[]).unwrap();
    billing.outside = true;
    let (reader, _) = reader_over(billing, Duration::from_secs(5));
    let base = serve(reader).await;
    let (status, reply) = post(&base, &request("q")).await;
    assert_eq!((status, error_of(&reply)), (502, ErrorCode::NoCost));
    assert!(
        reply.get("content").is_none(),
        "nothing comes back with it: {reply}"
    );
    // it asked for the accounting and for zero retention, as an outside upstream is always told
    let sent: Value = serde_json::from_slice(&stub.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        sent["provider"],
        json!({ "data_collection": "deny", "zdr": true })
    );
    assert_eq!(sent["usage"], json!({ "include": true }));

    // a private upstream that reports nothing costs nothing, and is answered
    let (base, _) = started(&stub, &[]).await;
    let (status, reply) = post(&base, &request("q")).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["cost_usd"], 0.0);
}

#[tokio::test]
async fn what_the_upstream_does_wrong_is_the_readers_error() {
    let cases: Vec<(&str, ResponseTemplate, ErrorCode)> = vec![
        (
            "a 500",
            ResponseTemplate::new(500).set_body_string("boom"),
            ErrorCode::UpstreamFailed,
        ),
        ("a 429", ResponseTemplate::new(429), ErrorCode::UpstreamFailed),
        (
            "a redirect",
            ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:9/"),
            ErrorCode::UpstreamFailed,
        ),
        (
            "another shape",
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true })),
            ErrorCode::UpstreamFailed,
        ),
        (
            "not JSON",
            ResponseTemplate::new(200).set_body_string("<html>"),
            ErrorCode::UpstreamFailed,
        ),
        (
            "no choice",
            ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })),
            ErrorCode::UpstreamFailed,
        ),
        (
            "an empty answer",
            ResponseTemplate::new(200).set_body_json(completion("  ", Some(0.0))),
            ErrorCode::UpstreamFailed,
        ),
        (
            "a negative cost",
            ResponseTemplate::new(200).set_body_json(completion("x", Some(-0.5))),
            ErrorCode::UpstreamFailed,
        ),
        (
            "an answer cut short",
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "content": "half" }, "finish_reason": "length" }],
                "usage": { "cost": 0.01 },
            })),
            ErrorCode::UpstreamFailed,
        ),
        (
            "a filtered answer",
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "content": "x" }, "finish_reason": "content_filter" }],
                "usage": { "cost": 0.01 },
            })),
            ErrorCode::UpstreamFailed,
        ),
        (
            "a slow answer",
            ResponseTemplate::new(200)
                .set_body_json(completion("x", Some(0.0)))
                .set_delay(Duration::from_millis(1_500)),
            ErrorCode::UpstreamTimeout,
        ),
    ];
    for (what, response, expected) in cases {
        let stub = MockServer::start().await;
        upstream_says(&stub, response).await;
        let (reader, _) = reader_over(
            config(&format!("{}/v1/chat/completions", stub.uri()), &[]).unwrap(),
            Duration::from_millis(400),
        );
        let base = serve(reader).await;
        let (status, reply) = post(&base, &request("q")).await;
        assert_eq!(
            (status, error_of(&reply)),
            (expected.status(), expected),
            "{what}"
        );
        // a failure charges nothing: the cap is still whole
        assert!(reply.get("cost_usd").is_none(), "{what}: {reply}");
    }
}

#[tokio::test]
async fn no_body_reaches_a_log_line() {
    let marker_question = "ZqQuestion-Marker-4471";
    let marker_content = "ZqContent-Marker-9902";
    let marker_error = "ZqErrorBody-Marker-3318";
    let stub = MockServer::start().await;
    upstream_says(
        &stub,
        ResponseTemplate::new(200).set_body_json(completion(marker_content, Some(0.01))),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let key = key_file(&dir, KEY);
    let (base, lines) = started(&stub, &[("READER_KEY_FILE", key.as_str())]).await;
    assert_eq!(post(&base, &request(marker_question)).await.0, 200);

    // a refused request, a too large one and an upstream that fails with a marker in its body
    let (status, _) = post_bytes(&base, marker_question.as_bytes().to_vec()).await;
    assert_eq!(status, 400);
    let _ = post_unread(&base, &request(&"x".repeat(MAX_BODY_BYTES))).await;
    logged(&lines, "413").await;
    let failing = MockServer::start().await;
    upstream_says(&failing, ResponseTemplate::new(500).set_body_string(marker_error)).await;
    let (failing_base, failing_lines) = started(&failing, &[("READER_KEY_FILE", key.as_str())]).await;
    assert_eq!(post(&failing_base, &request(marker_question)).await.0, 502);

    for (name, taken) in [("the first reader", &lines), ("the second", &failing_lines)] {
        let taken = taken.lock().unwrap();
        assert!(!taken.is_empty(), "{name} logged nothing");
        for line in taken.iter() {
            for marker in [marker_question, marker_content, marker_error, KEY] {
                assert!(
                    !line.contains(marker),
                    "{name}: a log line holds {marker}: {line}"
                );
            }
        }
    }
    // the status of a failure is logged, as its kind
    assert!(
        failing_lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("502") && line.contains("status 500"))
    );
    // and the start line names the upstream's host and nothing of its path
    let config = config("http://llm.internal:8000/secret/path?token=abc", &[]).unwrap();
    let start = reader::start_line(&config);
    assert!(
        start.contains("llm.internal") && start.contains("private") && !start.contains("secret"),
        "{start}"
    );
    assert!(!format!("{config:?}").contains(KEY));
}

// ---------------------------------------------------------------- the chain

/// The tools over the fixture, in `code_model`, calling `client`.
async fn tools_calling(client: mercury_probe::reader::client::ReaderClient) -> mercury_probe::tools::Tools {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
    let mut probe = mercury_probe::probe::Probe::dev(&repo, &repo.join("meta"), None).unwrap();
    probe.access = mercury_schema::AccessMode::CodeModel;
    let index = mercury_probe::index::Index::in_memory().await.unwrap();
    index
        .replace(&probe.rows("2026-09-30T12:05:00Z".parse().unwrap()).unwrap())
        .await
        .unwrap();
    mercury_probe::tools::Tools::new(index, &probe)
        .unwrap()
        .with_reader(client)
}

fn client_to(base: &str, timeout: Duration) -> mercury_probe::reader::client::ReaderClient {
    mercury_probe::reader::client::ReaderClient::new(
        &reqwest::Url::parse(base).unwrap(),
        MODEL.to_owned(),
        150,
        timeout,
    )
    .unwrap()
}

/// `ask_code` through the real reader to a stub upstream: the findings parse, the cost is the one
/// the stub reports, and the prompt's version is 1.
#[tokio::test]
async fn the_real_reader_chain_answers_ask_code() {
    let (upstream, stub) = support::upstream().await;
    let (reader, lines) = reader_over(
        config(&format!("{upstream}/v1/chat/completions"), &[]).unwrap(),
        Duration::from_secs(5),
    );
    let reader_url = serve(reader).await;
    let tools = tools_calling(client_to(&reader_url, Duration::from_secs(20))).await;

    let reply = tools
        .call("ask_code", json!({ "question": "How is the export built?" }))
        .await
        .unwrap();
    let reply: mercury_schema::facts::AskCodeReply = serde_json::from_value(reply).unwrap();
    assert_eq!(reply.prompt_version, 1);
    assert_eq!(reply.cost_usd.get(), 0.01, "the cost the upstream reported");
    for plain in support::PLAIN {
        assert!(
            reply
                .findings
                .iter()
                .any(|finding| finding.text.as_str() == plain),
            "{plain}"
        );
    }
    assert!(reply.withheld > 0, "the stub's leaks were counted");
    assert_eq!(stub.sent().len(), 1);
    // the reader saw the request and logged its size and cost, and nothing of it
    let line = lines.lock().unwrap().last().cloned().unwrap();
    assert!(
        line.starts_with("read: 200,") && line.contains("$0.0100"),
        "{line}"
    );
    assert!(!line.contains("export"), "{line}");
}

/// A stalled upstream ends in the reader's own 504 when the reader's clock is the shorter, and in
/// `serve`'s when its own is: either way the fixed error and no findings, and the other tools work.
#[tokio::test]
async fn a_stalled_upstream_ends_as_the_fixed_error_at_each_layer() {
    let stalled = MockServer::start().await;
    upstream_says(
        &stalled,
        ResponseTemplate::new(200)
            .set_body_json(completion("{\"findings\":[]}", Some(0.01)))
            .set_delay(Duration::from_millis(1_500)),
    )
    .await;
    let upstream = format!("{}/v1/chat/completions", stalled.uri());

    // the reader's clock is the shorter: it answers 504 itself
    let (reader, _) = reader_over(config(&upstream, &[]).unwrap(), Duration::from_millis(300));
    let reader_url = serve(reader).await;
    let (status, body) = post(&reader_url, &request("q")).await;
    assert_eq!((status, error_of(&body)), (504, ErrorCode::UpstreamTimeout));
    let tools = tools_calling(client_to(&reader_url, Duration::from_secs(5))).await;
    let err = tools
        .call("ask_code", json!({ "question": "How is the export built?" }))
        .await
        .unwrap_err();
    assert_eq!(err, mercury_probe::tools::ToolError::ReaderFailed);
    assert_eq!(err.to_string(), "the code reader gave no answer");

    // `serve`'s clock is the shorter: it gives up first
    let (reader, _) = reader_over(config(&upstream, &[]).unwrap(), Duration::from_secs(5));
    let reader_url = serve(reader).await;
    let tools = tools_calling(client_to(&reader_url, Duration::from_millis(300))).await;
    assert_eq!(
        tools
            .call("ask_code", json!({ "question": "How is the export built?" }))
            .await
            .unwrap_err(),
        mercury_probe::tools::ToolError::ReaderFailed
    );
    assert!(tools.call("health", json!({})).await.is_ok());
}
