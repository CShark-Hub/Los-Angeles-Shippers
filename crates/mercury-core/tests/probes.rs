//! Probes in the core: registration behind the probe token, the check of the tools a probe serves,
//! tool calls parsed strictly, quarantine, and heartbeats. A scripted stand-in plays every way a
//! probe can misbehave; the real probe shows the two sides speak the same MCP.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::TimeDelta;
use mercury_core::cli::ProbeCommand;
use mercury_core::db::{self, Db};
use mercury_core::error::{ApiError, ErrorCode};
use mercury_core::internal::{self, AllowList};
use mercury_core::probes::{
    self, McpClient, McpError, ProbeConfig, ProbeError, ProbeSecret, ProbeStatus, Probes, ToolReply, admin,
};
use mercury_probe::index::Index;
use mercury_probe::mcp::{ProbeToken, app};
use mercury_probe::probe::Probe;
use mercury_probe::registration;
use mercury_probe::tools::Tools;
use mercury_schema::api::ProbeRegistration;
use mercury_schema::redact::RedactRule;
use mercury_schema::{AccessMode, ToolName};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "probe-delta-token-for-tests";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

/// The real probe's registration of the fixture, in `access` mode, naming the five tools without
/// `changes`, one of the tool sets the core accepts: the stand-in below serves those five.
fn registration(access: Option<AccessMode>) -> ProbeRegistration {
    let mut body = real_registration(access);
    body.tools = FIVE
        .iter()
        .map(|tool| serde_json::from_value(json!(tool)).unwrap())
        .collect();
    body
}

/// The real probe's own registration of the fixture: all six tools.
fn real_registration(access: Option<AccessMode>) -> ProbeRegistration {
    let repo = fixture();
    let probe = Probe::dev(&repo, &repo.join("meta"), access).unwrap();
    registration::body(&probe)
}

// ---------------------------------------------------------------- a scripted probe

/// What the stand-in answers to `tools/call`.
#[derive(Clone)]
enum Answer {
    /// This structured content, in a JSON reply.
    Structured(Value),
    /// The same, as an event stream with a notification first.
    Streamed(Value),
    /// A JSON reply over the size cap.
    Oversized,
    /// A tool failure (`isError: true`).
    ToolError,
    /// A JSON-RPC error.
    RpcError,
    /// This structured content, after `delay`.
    Slow(Value, Duration),
}

#[derive(Clone)]
struct Script {
    tools: Vec<&'static str>,
    answer: Arc<Mutex<Answer>>,
    calls: Arc<Mutex<usize>>,
    tokens: Arc<Mutex<Vec<Option<String>>>>,
}

impl Script {
    fn new(tools: &[&'static str], answer: Answer) -> Self {
        Self {
            tools: tools.to_vec(),
            answer: Arc::new(Mutex::new(answer)),
            calls: Arc::default(),
            tokens: Arc::default(),
        }
    }

    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

const FIVE: [&str; 5] = ["describe", "progress", "tested", "blocked", "health"];

fn rpc(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

async fn scripted(State(script): State<Script>, headers: HeaderMap, body: axum::Json<Value>) -> Response {
    script.tokens.lock().unwrap().push(
        headers
            .get(AUTHORIZATION)
            .map(|value| value.to_str().unwrap().to_owned()),
    );
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    match body["method"].as_str().unwrap() {
        "initialize" => axum::Json(rpc(
            &id,
            json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                   "serverInfo": {"name": "scripted", "version": "0"}}),
        ))
        .into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let tools: Vec<Value> = script
                .tools
                .iter()
                .map(|name| json!({"name": name, "inputSchema": {"type": "object"}}))
                .collect();
            axum::Json(rpc(&id, json!({ "tools": tools }))).into_response()
        }
        "tools/call" => {
            *script.calls.lock().unwrap() += 1;
            let answer = script.answer.lock().unwrap().clone();
            match answer {
                Answer::Structured(content) => {
                    axum::Json(rpc(&id, json!({"content": [], "structuredContent": content}))).into_response()
                }
                Answer::Streamed(content) => {
                    let note = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
                    let reply = rpc(&id, json!({"content": [], "structuredContent": content}));
                    let stream = format!("event: message\ndata: {note}\n\nevent: message\ndata: {reply}\n\n");
                    ([(CONTENT_TYPE, "text/event-stream")], stream).into_response()
                }
                Answer::Oversized => {
                    let content = json!({"padding": "x".repeat(300 * 1024)});
                    axum::Json(rpc(&id, json!({"content": [], "structuredContent": content}))).into_response()
                }
                Answer::ToolError => {
                    axum::Json(rpc(&id, json!({"content": [{"type": "text", "text": "no index"}], "isError": true})))
                        .into_response()
                }
                Answer::RpcError => axum::Json(json!({"jsonrpc": "2.0", "id": id,
                                                      "error": {"code": -32602, "message": "invalid params"}}))
                .into_response(),
                Answer::Slow(content, delay) => {
                    tokio::time::sleep(delay).await;
                    axum::Json(rpc(&id, json!({"content": [], "structuredContent": content}))).into_response()
                }
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn scripted_probe(script: &Script) -> String {
    let url = serve(
        Router::new()
            .route("/mcp", post(scripted))
            .with_state(script.clone()),
    )
    .await;
    format!("{url}/mcp")
}

/// A `health` reply as the schema has it.
fn health() -> Value {
    json!({
        "schema": "mercury.facts/0.1", "as_of": "2026-10-02T10:00:00Z",
        "build": {"status": "passed", "at": "2026-10-02T09:00:00Z"},
        "ci_pass_rate_7d": 0.9, "open_prs": {"count": 2, "oldest_days": 3},
        "todo_fixme": {"count": 4, "trend_14d": -1},
        "last_release": {"name": null, "at": null}, "score": 81
    })
}

// ---------------------------------------------------------------- the core's side

struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    probes: Probes,
}

/// The core with one probe, `delta`, at `url`, capped at `access_max`, whose calls time out after
/// `timeout`.
async fn core(url: &str, access_max: AccessMode, timeout: Duration) -> Core {
    core_timed(url, access_max, timeout, timeout).await
}

/// The same, with a time of its own for the code reader's call.
async fn core_timed(
    url: &str,
    access_max: AccessMode,
    timeout: Duration,
    ask_code_timeout: Duration,
) -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let http = reqwest::Client::builder().timeout(timeout).build().unwrap();
    let config = ProbeConfig {
        project: "delta".into(),
        url: url.into(),
        token: ProbeSecret::new(TOKEN).unwrap(),
        access_max,
    };
    let probes = Probes::with_timeouts(db.clone(), http, vec![config], timeout, ask_code_timeout);
    Core {
        _dir: dir,
        db,
        probes,
    }
}

async fn status(db: &Db) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, quarantine_reason FROM probes WHERE project = 'delta'")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Registered and checked: a probe the core calls.
async fn registered(core: &Core) {
    let recorded = core.probes.record(&registration(None), db::now()).await.unwrap();
    assert_eq!(recorded, probes::Recorded::ToCheck);
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);
}

fn assert_quarantined_for(result: Result<ToolReply, ProbeError>) {
    match result {
        Err(err @ ProbeError::BrokeContract(_)) => {
            let error = ApiError::from(err);
            assert_eq!(error.code, ErrorCode::ProbeUnavailable);
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        }
        other => panic!("expected the probe to break the contract, got {other:?}"),
    }
}

#[tokio::test]
async fn a_reply_with_an_extra_field_quarantines_the_probe_and_fails_the_call_with_probe_unavailable() {
    let mut extra = health();
    extra["source_path"] = json!("src/export/csv.rs");
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    assert!(matches!(
        core.probes
            .call("delta", ToolName::Health, json!({}))
            .await
            .unwrap(),
        ToolReply::Health(_)
    ));

    script.answer(Answer::Structured(extra));
    assert_quarantined_for(core.probes.call("delta", ToolName::Health, json!({})).await);
    let (status, reason) = status(&core.db).await;
    assert_eq!(status, "quarantined");
    assert_eq!(reason.as_deref(), Some("a health reply doesn't match its schema"));
    assert_eq!(
        audit_rows(&core.db).await,
        [(
            "probe_quarantined".to_owned(),
            false,
            Some("delta".to_owned()),
            json!({ "by": "check", "reason": "a health reply doesn't match its schema" })
        )],
        "the quarantine is in the audit"
    );
    assert!(
        !reason.unwrap().contains("src/export"),
        "the reason never repeats the reply"
    );

    // never called again until released
    let calls = script.calls();
    assert!(matches!(
        core.probes.call("delta", ToolName::Health, json!({})).await,
        Err(ProbeError::Quarantined)
    ));
    assert_eq!(script.calls(), calls);
}

#[tokio::test]
async fn a_probe_serving_an_extra_tool_is_quarantined_at_its_check() {
    let script = Script::new(
        &["describe", "progress", "tested", "blocked", "health", "read_file"],
        Answer::Structured(health()),
    );
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    core.probes.record(&registration(None), db::now()).await.unwrap();
    assert_eq!(
        core.probes.check("delta").await.unwrap(),
        ProbeStatus::Quarantined
    );
    assert_eq!(
        status(&core.db).await,
        (
            "quarantined".to_owned(),
            Some("it serves a tool mercury.facts/0.1 doesn't have".to_owned())
        )
    );
    assert!(matches!(
        core.probes.call("delta", ToolName::Health, json!({})).await,
        Err(ProbeError::Quarantined)
    ));
    assert_eq!(script.calls(), 0);
}

#[tokio::test]
async fn a_tool_set_other_than_the_schemas_or_the_registered_one_is_quarantined() {
    for (served, reason) in [
        (&FIVE[..4], "it serves another tool set than mercury.facts/0.1"),
        (
            &["describe", "progress", "tested", "blocked", "health", "health"][..],
            "it serves another tool set than mercury.facts/0.1",
        ),
        (
            &["describe", "progress", "tested", "blocked", "health", "changes"][..],
            "it serves other tools than it registered",
        ),
    ] {
        let script = Script::new(served, Answer::Structured(health()));
        let core = core(
            &scripted_probe(&script).await,
            AccessMode::Code,
            Duration::from_secs(5),
        )
        .await;
        core.probes.record(&registration(None), db::now()).await.unwrap();
        assert_eq!(
            core.probes.check("delta").await.unwrap(),
            ProbeStatus::Quarantined,
            "{served:?}"
        );
        assert_eq!(status(&core.db).await.1.as_deref(), Some(reason), "{served:?}");
    }
}

#[tokio::test]
async fn a_reply_that_breaks_the_schema_or_the_size_cap_quarantines_the_probe() {
    let mut missing = health();
    missing.as_object_mut().unwrap().remove("score");
    let mut other_schema = health();
    other_schema["schema"] = json!("mercury.facts/0.2");
    let mut code = health();
    code["build"]["status"] = json!("fn export() { … }");
    for (answer, reason) in [
        (
            Answer::Structured(missing),
            "a health reply doesn't match its schema",
        ),
        (
            Answer::Structured(other_schema),
            "a health reply doesn't match its schema",
        ),
        (
            Answer::Structured(code),
            "a health reply doesn't match its schema",
        ),
        (
            Answer::Structured(json!("not an object")),
            "a health reply doesn't match its schema",
        ),
        (
            Answer::Oversized,
            "a health reply: the probe's reply is over 256 KB",
        ),
    ] {
        let script = Script::new(&FIVE, answer);
        let core = core(
            &scripted_probe(&script).await,
            AccessMode::Code,
            Duration::from_secs(5),
        )
        .await;
        registered(&core).await;
        assert_quarantined_for(core.probes.call("delta", ToolName::Health, json!({})).await);
        assert_eq!(
            status(&core.db).await,
            ("quarantined".to_owned(), Some(reason.to_owned()))
        );
    }
}

#[tokio::test]
async fn a_timeout_fails_only_that_call_and_quarantines_nothing() {
    let script = Script::new(&FIVE, Answer::Slow(health(), Duration::from_millis(1500)));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_millis(300),
    )
    .await;
    registered(&core).await;
    match core.probes.call("delta", ToolName::Health, json!({})).await {
        Err(err @ ProbeError::Unavailable(McpError::Timeout)) => {
            assert_eq!(ApiError::from(err).code, ErrorCode::ProbeUnavailable);
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(status(&core.db).await, ("healthy".to_owned(), None));
    script.answer(Answer::Structured(health()));
    assert!(
        core.probes
            .call("delta", ToolName::Health, json!({}))
            .await
            .is_ok(),
        "the next call works"
    );
}

const SEVEN: [&str; 7] = [
    "describe", "progress", "tested", "blocked", "health", "changes", "ask_code",
];

/// The registration of a probe in `access` that serves the seven tools of the code reader.
fn seven_tool_registration(access: AccessMode) -> ProbeRegistration {
    let mut body = real_registration(None);
    body.access = access;
    body.tools = SEVEN
        .iter()
        .map(|tool| serde_json::from_value(json!(tool)).unwrap())
        .collect();
    body
}

/// What `ask_code` replies: one finding, and what the reader's model cost.
fn findings() -> Value {
    json!({
        "schema": "mercury.facts/0.1", "as_of": "2026-10-04T15:12:30Z",
        "findings": [{ "text": "The export writes one file per invoice batch", "module": "Export", "confidence": 82 }],
        "withheld": 0, "cost_usd": 0.0031, "prompt_version": 1
    })
}

#[tokio::test]
async fn a_probe_serving_seven_tools_below_code_model_is_quarantined() {
    for access in [AccessMode::Metadata, AccessMode::Code] {
        let script = Script::new(&SEVEN, Answer::Structured(findings()));
        let core = core(
            &scripted_probe(&script).await,
            AccessMode::Code,
            Duration::from_secs(5),
        )
        .await;
        core.probes
            .record(&seven_tool_registration(access), db::now())
            .await
            .unwrap();
        assert_eq!(
            core.probes.check("delta").await.unwrap(),
            ProbeStatus::Quarantined,
            "{access:?}"
        );
        // the reason is the one a probe with any other tool set gets, and no call is made
        assert_eq!(
            status(&core.db).await,
            (
                "quarantined".to_owned(),
                Some("it serves another tool set than mercury.facts/0.1".to_owned())
            ),
            "{access:?}"
        );
        assert!(matches!(
            core.probes.call("delta", ToolName::AskCode, json!({})).await,
            Err(ProbeError::Quarantined)
        ));
        assert_eq!(script.calls(), 0);
    }
}

#[tokio::test]
async fn a_code_model_probe_serving_seven_tools_is_healthy() {
    let script = Script::new(&SEVEN, Answer::Structured(findings()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::CodeModel,
        Duration::from_secs(5),
    )
    .await;
    core.probes
        .record(&seven_tool_registration(AccessMode::CodeModel), db::now())
        .await
        .unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);
    assert_eq!(status(&core.db).await, ("healthy".to_owned(), None));
    let reply = core
        .probes
        .call(
            "delta",
            ToolName::AskCode,
            json!({ "question": "How does the export pick its format?" }),
        )
        .await
        .unwrap();
    let ToolReply::AskCode(reply) = reply else {
        panic!("the reader's reply is an ask_code reply")
    };
    assert_eq!(reply.findings.len(), 1);

    // the same probe serving only the six is as healthy: the reader's tool is its own to add
    let script = Script::new(
        &["describe", "progress", "tested", "blocked", "health", "changes"],
        Answer::Structured(health()),
    );
    let without_reader = self::core(
        &scripted_probe(&script).await,
        AccessMode::CodeModel,
        Duration::from_secs(5),
    )
    .await;
    let mut six = real_registration(None);
    six.access = AccessMode::CodeModel;
    without_reader.probes.record(&six, db::now()).await.unwrap();
    assert_eq!(
        without_reader.probes.check("delta").await.unwrap(),
        ProbeStatus::Healthy
    );
}

#[tokio::test]
async fn ask_code_waits_longer_than_the_other_tools() {
    // a reply after 700 ms: too slow for the lookups' 300 ms, in time for the reader's 3 s
    let slow = Duration::from_millis(700);
    let script = Script::new(&SEVEN, Answer::Slow(findings(), slow));
    let core = core_timed(
        &scripted_probe(&script).await,
        AccessMode::CodeModel,
        Duration::from_millis(300),
        Duration::from_secs(3),
    )
    .await;
    core.probes
        .record(&seven_tool_registration(AccessMode::CodeModel), db::now())
        .await
        .unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);
    let reply = core
        .probes
        .call(
            "delta",
            ToolName::AskCode,
            json!({ "question": "Why is the export slow?" }),
        )
        .await;
    assert!(
        matches!(reply, Ok(ToolReply::AskCode(_))),
        "the reader had time: {reply:?}"
    );
    // any other tool gives up after the lookups' time, and quarantines nothing
    script.answer(Answer::Slow(health(), slow));
    match core.probes.call("delta", ToolName::Health, json!({})).await {
        Err(ProbeError::Unavailable(McpError::Timeout)) => {}
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(status(&core.db).await, ("healthy".to_owned(), None));

    // and the reader that is slower than its own time fails that call alone, like any tool
    let script = Script::new(&SEVEN, Answer::Slow(findings(), slow));
    let core = core_timed(
        &scripted_probe(&script).await,
        AccessMode::CodeModel,
        Duration::from_secs(5),
        Duration::from_millis(300),
    )
    .await;
    core.probes
        .record(&seven_tool_registration(AccessMode::CodeModel), db::now())
        .await
        .unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);
    match core
        .probes
        .call(
            "delta",
            ToolName::AskCode,
            json!({ "question": "Why is the export slow?" }),
        )
        .await
    {
        Err(ProbeError::Unavailable(McpError::Timeout)) => {}
        other => panic!("expected the reader's timeout, got {other:?}"),
    }
    assert_eq!(status(&core.db).await, ("healthy".to_owned(), None));
}

#[tokio::test]
async fn a_tool_failure_or_a_refused_call_fails_the_call_without_quarantine() {
    for (answer, expected) in [
        (Answer::ToolError, McpError::ToolFailed),
        (Answer::RpcError, McpError::Rpc(-32602)),
    ] {
        let script = Script::new(&FIVE, answer);
        let core = core(
            &scripted_probe(&script).await,
            AccessMode::Code,
            Duration::from_secs(5),
        )
        .await;
        registered(&core).await;
        match core.probes.call("delta", ToolName::Health, json!({})).await {
            Err(ProbeError::Unavailable(err)) => assert_eq!(err, expected),
            other => panic!("expected {expected}, got {other:?}"),
        }
        assert_eq!(status(&core.db).await.0, "healthy");
    }
}

#[tokio::test]
async fn a_reply_as_an_event_stream_is_read_like_one_in_json() {
    let script = Script::new(&FIVE, Answer::Streamed(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    assert!(matches!(
        core.probes
            .call("delta", ToolName::Health, json!({}))
            .await
            .unwrap(),
        ToolReply::Health(_)
    ));
}

#[tokio::test]
async fn a_tool_the_probe_doesnt_serve_is_never_asked_for() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    assert!(matches!(
        core.probes
            .call("delta", ToolName::Changes, json!({"since": "2026-09-01"}))
            .await,
        Err(ProbeError::NotServed)
    ));
    assert!(matches!(
        core.probes.call("omega", ToolName::Health, json!({})).await,
        Err(ProbeError::Unknown)
    ));
    assert_eq!(script.calls(), 0);
}

#[tokio::test]
async fn a_mode_above_access_max_is_quarantined_at_registration_without_a_call() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Metadata,
        Duration::from_secs(5),
    )
    .await;
    let recorded = core
        .probes
        .record(&registration(Some(AccessMode::Code)), db::now())
        .await
        .unwrap();
    assert_eq!(
        recorded,
        probes::Recorded::Quarantined("it reports an access mode above the project's access_max".into())
    );
    assert_eq!(
        core.probes.check("delta").await.unwrap(),
        ProbeStatus::Quarantined
    );
    assert!(
        script.tokens.lock().unwrap().is_empty(),
        "the probe was never called"
    );

    // and it stays quarantined, even when it lowers its mode, until an admin releases it
    let lowered = core
        .probes
        .record(&registration(Some(AccessMode::Metadata)), db::now())
        .await
        .unwrap();
    assert!(matches!(lowered, probes::Recorded::Quarantined(_)));

    // the registration that quarantined it is in the audit, once: a heartbeat of a probe that is
    // quarantined already writes no row
    assert_eq!(
        audit_rows(&core.db).await,
        [(
            "probe_quarantined".to_owned(),
            false,
            Some("delta".to_owned()),
            json!({ "by": "registration", "reason": "it reports an access mode above the project's access_max" })
        )],
        "the quarantine at registration is in the audit"
    );
}

/// A manifest whose redaction pattern doesn't compile would fail every question about the project,
/// so the registration quarantines the probe, with a fixed reason that never repeats the pattern.
#[tokio::test]
async fn a_manifest_with_a_pattern_that_doesnt_compile_is_quarantined_at_registration() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    let mut broken = registration(None);
    broken.manifest.redact.push(RedactRule {
        pattern: "(secret-client".into(),
        replace: "the client".into(),
    });
    let recorded = core.probes.record(&broken, db::now()).await.unwrap();
    let reason = "its manifest has a redaction pattern that doesn't compile";
    assert_eq!(recorded, probes::Recorded::Quarantined(reason.into()));
    assert_eq!(
        status(&core.db).await,
        ("quarantined".to_owned(), Some(reason.to_owned()))
    );
    assert_eq!(
        core.probes.check("delta").await.unwrap(),
        ProbeStatus::Quarantined
    );
    assert!(script.tokens.lock().unwrap().is_empty(), "never called");
    let rows = audit_rows(&core.db).await;
    assert_eq!(
        rows,
        [(
            "probe_quarantined".to_owned(),
            false,
            Some("delta".to_owned()),
            json!({ "by": "registration", "reason": reason })
        )]
    );
    assert!(
        !rows[0].3.to_string().contains("secret-client"),
        "the audit never repeats the pattern"
    );

    // the manifest as it was, registered again, doesn't free it: only an admin does
    let again = core.probes.record(&registration(None), db::now()).await.unwrap();
    assert!(matches!(again, probes::Recorded::Quarantined(_)));
    assert_eq!(audit_rows(&core.db).await.len(), 1);
}

/// When the audit row of a registration's quarantine can't be written, the registration isn't
/// recorded either: the probe keeps its last state, and the next registration tries again.
#[tokio::test]
async fn a_quarantine_at_registration_whose_audit_row_cant_be_written_is_not_recorded() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Metadata,
        Duration::from_secs(5),
    )
    .await;
    // made and dropped on one connection of its own, as in the probe commands' test below
    let mut ddl = core.db.pool().acquire().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER no_quarantine BEFORE INSERT ON audit_events WHEN NEW.type = 'probe_quarantined' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(&mut *ddl)
    .await
    .unwrap();
    let above = registration(Some(AccessMode::Code));
    assert!(matches!(
        core.probes.record(&above, db::now()).await,
        Err(ProbeError::Audit(_))
    ));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probes")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "no probe row without its audit row");

    sqlx::query("DROP TRIGGER no_quarantine")
        .execute(&mut *ddl)
        .await
        .unwrap();
    assert!(matches!(
        core.probes.record(&above, db::now()).await.unwrap(),
        probes::Recorded::Quarantined(_)
    ));
    assert_eq!(audit_rows(&core.db).await.len(), 1);
}

#[tokio::test]
async fn a_probe_turns_stale_three_minutes_after_its_last_registration() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    let registered_at = db::parse_time("2026-10-02T10:00:00.000Z").unwrap();
    core.probes
        .record(&registration(None), registered_at)
        .await
        .unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);

    let just_before = registered_at + TimeDelta::minutes(3) - TimeDelta::milliseconds(1);
    assert_eq!(core.probes.sweep_stale(just_before).await.unwrap(), 0);
    assert_eq!(status(&core.db).await.0, "healthy");
    let lines = admin::list(&core.db, just_before).await.unwrap();
    assert_eq!(lines[0].status, ProbeStatus::Healthy);

    let three_minutes_on = registered_at + TimeDelta::minutes(3);
    assert_eq!(
        admin::list(&core.db, three_minutes_on).await.unwrap()[0].status,
        ProbeStatus::Stale,
        "the listing knows before the sweep"
    );
    assert_eq!(core.probes.sweep_stale(three_minutes_on).await.unwrap(), 1);
    assert_eq!(status(&core.db).await.0, "stale");

    // a new registration and check make it healthy again
    core.probes
        .record(&registration(None), three_minutes_on)
        .await
        .unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Healthy);
}

#[tokio::test]
async fn a_probe_that_cant_be_reached_for_its_check_stays_unchecked() {
    let core = core("http://127.0.0.1:9/mcp", AccessMode::Code, Duration::from_secs(5)).await;
    core.probes.record(&registration(None), db::now()).await.unwrap();
    assert_eq!(core.probes.check("delta").await.unwrap(), ProbeStatus::Stale);
    assert_eq!(status(&core.db).await, ("stale".to_owned(), None));
}

// ---------------------------------------------------------------- the registration route

/// A request from `source` to the internal listener with the registration route.
async fn send(probes: &Probes, source: &str, token: Option<&str>, body: Body) -> StatusCode {
    let router = internal::router_with(
        AllowList::parse(Some("127.0.0.0/8")).unwrap(),
        probes::routes(probes.clone()),
    );
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/probes/register")
        .header(CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = request.body(body).unwrap();
    let source: SocketAddr = source.parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(source));
    router.oneshot(request).await.unwrap().status()
}

fn json_body(value: &impl serde::Serialize) -> Body {
    Body::from(serde_json::to_vec(value).unwrap())
}

/// The registration route reads at most 64 KB, and refuses more before it reads a byte: with the
/// probe's token or without it, and nothing is recorded.
#[tokio::test]
async fn a_registration_over_64_kb_is_refused_before_anything_reads_it() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    let local = "127.0.0.1:40000";
    let cap = mercury_core::limits::MAX_BODY;
    assert_eq!(cap, 64 * 1024, "the cap the contract names");
    for token in [Some(TOKEN), None] {
        assert_eq!(
            send(&core.probes, local, token, Body::from(vec![b'a'; cap + 1])).await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "a body of one byte over the cap, authenticated {}",
            token.is_some()
        );
    }
    // the cap itself is allowed: it is read, and is no registration
    assert_eq!(
        send(&core.probes, local, Some(TOKEN), Body::from(vec![b'a'; cap])).await,
        StatusCode::BAD_REQUEST
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probes")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "nothing was recorded");
}

#[tokio::test]
async fn the_registration_route_takes_the_probes_token_and_checks_it_afterwards() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    let body = registration(None);
    let local = "127.0.0.1:40000";

    assert_eq!(
        send(&core.probes, local, None, json_body(&body)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&core.probes, local, Some("another-token"), json_body(&body)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &core.probes,
            local,
            Some(TOKEN),
            Body::from("{\"project\": \"delta\"}")
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    let mut other = serde_json::to_value(&body).unwrap();
    other["project"] = json!("omega");
    assert_eq!(
        send(&core.probes, local, Some(TOKEN), json_body(&other)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&core.probes, "10.1.2.3:40000", Some(TOKEN), json_body(&body)).await,
        StatusCode::FORBIDDEN,
        "the source check comes first"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probes")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "nothing recorded so far");

    assert_eq!(
        send(&core.probes, local, Some(TOKEN), json_body(&body)).await,
        StatusCode::ACCEPTED
    );
    for _ in 0..100 {
        if status(&core.db).await.0 == "healthy" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        status(&core.db).await,
        ("healthy".to_owned(), None),
        "checked after the answer"
    );
    let tokens = script.tokens.lock().unwrap().clone();
    assert!(
        !tokens.is_empty()
            && tokens
                .iter()
                .all(|token| token.as_deref() == Some(&*format!("Bearer {TOKEN}")))
    );
}

#[tokio::test]
async fn a_quarantined_probe_hears_409_when_it_registers() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Metadata,
        Duration::from_secs(5),
    )
    .await;
    let body = registration(Some(AccessMode::Code));
    assert_eq!(
        send(&core.probes, "127.0.0.1:40000", Some(TOKEN), json_body(&body)).await,
        StatusCode::CONFLICT
    );
}

// ---------------------------------------------------------------- the real probe

/// The real probe on the fixture, serving MCP behind `TOKEN`.
async fn real_probe() -> String {
    let repo = fixture();
    let probe = Probe::dev(&repo, &repo.join("meta"), None).unwrap();
    let index = Index::in_memory().await.unwrap();
    index
        .replace(&probe.rows("2026-09-30T12:05:00Z".parse().unwrap()).unwrap())
        .await
        .unwrap();
    let tools = Tools::new(index, &probe).unwrap();
    let url = serve(app(tools, ProbeToken::new(Some(TOKEN)))).await;
    format!("{url}/mcp")
}

#[tokio::test]
async fn the_real_probe_registers_passes_its_check_and_answers_every_tool() {
    let url = real_probe().await;
    let core = core(&url, AccessMode::Code, Duration::from_secs(5)).await;

    // the probe's own registration loop, against the core's internal listener
    let internal = internal::router_with(
        AllowList::parse(Some("127.0.0.0/8")).unwrap(),
        probes::routes(core.probes.clone()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            internal.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let task = tokio::spawn(registration::keep_registering(
        format!("http://{addr}"),
        Some(TOKEN.to_owned()),
        registration::fixed(real_registration(None)),
        Duration::from_secs(60),
    ));
    for _ in 0..200 {
        if sqlx::query_scalar::<_, String>("SELECT status FROM probes WHERE project = 'delta'")
            .fetch_optional(core.db.pool())
            .await
            .unwrap()
            .as_deref()
            == Some("healthy")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    task.abort();
    assert_eq!(status(&core.db).await, ("healthy".to_owned(), None));

    let ToolReply::Describe(describe) = core
        .probes
        .call("delta", ToolName::Describe, json!({}))
        .await
        .unwrap()
    else {
        panic!("not a describe reply");
    };
    assert_eq!(describe.project.as_str(), "delta");
    for (tool, arguments) in [
        (ToolName::Progress, json!({"limit": 5})),
        (ToolName::Tested, json!({})),
        (ToolName::Blocked, json!({})),
        (ToolName::Health, json!({})),
        (ToolName::Changes, json!({"since": "2026-09-21"})),
    ] {
        let reply = core.probes.call("delta", tool, arguments).await;
        assert!(reply.is_ok(), "{tool:?}: {reply:?}");
    }
    // the sixth tool: every module, each with its commits and merged pull requests since the date
    let Ok(ToolReply::Changes(changes)) = core
        .probes
        .call("delta", ToolName::Changes, json!({"since": "2026-09-21"}))
        .await
    else {
        panic!("not a changes reply");
    };
    let counts: Vec<(&str, u32, u32)> = changes
        .modules
        .iter()
        .map(|m| (m.module.as_str(), m.commits, m.merged_prs))
        .collect();
    assert_eq!(
        counts,
        [
            ("Invoicing", 2, 2),
            ("Export", 2, 1),
            ("Auth", 0, 0),
            ("Other", 3, 1)
        ]
    );
    assert_eq!(status(&core.db).await.0, "healthy", "every reply met the schema");
}

#[tokio::test]
async fn the_wrong_token_keeps_the_real_probe_shut() {
    let url = real_probe().await;
    let client = McpClient::new(
        McpClient::http().unwrap(),
        url,
        ProbeSecret::new("not-the-token").unwrap(),
    );
    assert_eq!(client.initialize().await, Err(McpError::Status(401)));
}

// ---------------------------------------------------------------- the admin commands

#[tokio::test]
async fn probe_list_quarantine_and_release() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        admin::render(&admin::list(&core.db, db::now()).await.unwrap()),
        "No probe has registered.\n"
    );
    registered(&core).await;

    let shown = admin::render(&admin::list(&core.db, db::now()).await.unwrap());
    let row: Vec<&str> = shown.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(&row[..2], ["delta", "healthy"]);
    assert_eq!(row[2], "code", "{shown}");

    assert_eq!(
        admin::quarantine(&core.db, "delta", "reads more than agreed", db::now())
            .await
            .unwrap(),
        admin::Switched::Now
    );
    assert_eq!(
        admin::quarantine(&core.db, "delta", "again", db::now())
            .await
            .unwrap(),
        admin::Switched::Already
    );
    assert!(matches!(
        core.probes.call("delta", ToolName::Health, json!({})).await,
        Err(ProbeError::Quarantined)
    ));
    let shown = admin::render(&admin::list(&core.db, db::now()).await.unwrap());
    assert!(
        shown.contains("quarantined") && shown.contains("an admin: reads more than agreed"),
        "{shown}"
    );

    assert_eq!(
        admin::release(&core.db, "delta", db::now()).await.unwrap(),
        admin::Switched::Now
    );
    assert_eq!(
        admin::release(&core.db, "delta", db::now()).await.unwrap(),
        admin::Switched::Already
    );
    assert_eq!(
        status(&core.db).await,
        ("stale".to_owned(), None),
        "unchecked until it registers"
    );
    assert!(
        core.probes
            .call("delta", ToolName::Health, json!({}))
            .await
            .is_ok(),
        "called again"
    );

    for (result, case) in [
        (
            admin::quarantine(&core.db, "omega", "x", db::now())
                .await
                .map(|_| ()),
            "a project without a probe",
        ),
        (
            admin::quarantine(&core.db, "Delta!", "x", db::now())
                .await
                .map(|_| ()),
            "a project id of the wrong shape",
        ),
        (
            admin::quarantine(&core.db, "delta", " ", db::now())
                .await
                .map(|_| ()),
            "no reason",
        ),
        (
            admin::release(&core.db, "omega", db::now()).await.map(|_| ()),
            "releasing a project without a probe",
        ),
    ] {
        assert!(result.is_err(), "{case}");
    }
}

/// The probe commands change the probe and write their audit row in one transaction: when the row
/// can't be written, the probe is as it was and the command fails.
#[tokio::test]
async fn a_probe_switch_whose_audit_row_cant_be_written_is_not_made() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    // The triggers are made and dropped on one connection of their own, each under the name of the
    // event it refuses: a trigger made again under the name just dropped, on whichever connection
    // the pool hands out, could meet that connection's view of the schema from before the drop.
    let mut ddl = core.db.pool().acquire().await.unwrap();
    let refuse = |event: &'static str| {
        format!(
            "CREATE TRIGGER refuse_{event} BEFORE INSERT ON audit_events WHEN NEW.type = '{event}' \
             BEGIN SELECT RAISE(ABORT, 'the disk is full'); END"
        )
    };

    sqlx::query(sqlx::AssertSqlSafe(refuse("probe_quarantined")))
        .execute(&mut *ddl)
        .await
        .unwrap();
    let refused = admin::quarantine(&core.db, "delta", "reads more than agreed", db::now()).await;
    assert!(
        matches!(refused, Err(mercury_core::admin::AdminError::Audit(_))),
        "{refused:?}"
    );
    assert_eq!(
        status(&core.db).await,
        ("healthy".to_owned(), None),
        "not quarantined"
    );
    sqlx::query("DROP TRIGGER refuse_probe_quarantined")
        .execute(&mut *ddl)
        .await
        .unwrap();
    admin::quarantine(&core.db, "delta", "reads more than agreed", db::now())
        .await
        .unwrap();

    sqlx::query(sqlx::AssertSqlSafe(refuse("probe_released")))
        .execute(&mut *ddl)
        .await
        .unwrap();
    let refused = admin::release(&core.db, "delta", db::now()).await;
    assert!(
        matches!(refused, Err(mercury_core::admin::AdminError::Audit(_))),
        "{refused:?}"
    );
    assert_eq!(
        status(&core.db).await,
        (
            "quarantined".to_owned(),
            Some("an admin: reads more than agreed".to_owned())
        ),
        "still quarantined"
    );
}

/// A reply that breaks the contract quarantines the probe and writes the row in one transaction:
/// when the row can't be written the call fails, the probe is as it was, and the next reply that
/// breaks the contract tries again.
#[tokio::test]
async fn a_quarantine_by_a_check_whose_audit_row_cant_be_written_is_not_made() {
    let mut extra = health();
    extra["source_path"] = json!("src/export/csv.rs");
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    script.answer(Answer::Structured(extra));
    // made and dropped on one connection of its own, as in the test above
    let mut ddl = core.db.pool().acquire().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse BEFORE INSERT ON audit_events WHEN NEW.type = 'probe_quarantined' \n         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(&mut *ddl)
    .await
    .unwrap();

    let refused = core.probes.call("delta", ToolName::Health, json!({})).await;
    assert!(matches!(refused, Err(ProbeError::Audit(_))), "{refused:?}");
    assert_eq!(
        status(&core.db).await,
        ("healthy".to_owned(), None),
        "not quarantined without its row"
    );
    assert!(audit_rows(&core.db).await.is_empty());

    sqlx::query("DROP TRIGGER refuse")
        .execute(&mut *ddl)
        .await
        .unwrap();
    assert_quarantined_for(core.probes.call("delta", ToolName::Health, json!({})).await);
    assert_eq!(status(&core.db).await.0, "quarantined");
    assert_eq!(audit_rows(&core.db).await.len(), 1);
}

/// Every audit row: its type, whether it has a question, its project and its payload.
async fn audit_rows(db: &Db) -> Vec<(String, bool, Option<String>, Value)> {
    sqlx::query_as::<_, (String, Option<String>, Option<String>, String)>(
        "SELECT type, question_id, project, payload_json FROM audit_events ORDER BY seq",
    )
    .fetch_all(db.pool())
    .await
    .unwrap()
    .into_iter()
    .map(|(t, q, p, payload)| (t, q.is_some(), p, serde_json::from_str(&payload).unwrap()))
    .collect()
}

#[tokio::test]
async fn each_probe_command_leaves_one_audit_row() {
    let script = Script::new(&FIVE, Answer::Structured(health()));
    let core = core(
        &scripted_probe(&script).await,
        AccessMode::Code,
        Duration::from_secs(5),
    )
    .await;
    registered(&core).await;
    let delta = || "delta".to_owned();
    for command in [
        ProbeCommand::List,
        ProbeCommand::Quarantine {
            project: delta(),
            reason: "reads more than agreed".to_owned(),
        },
        ProbeCommand::Release { project: delta() },
    ] {
        admin::run(command, &core.db, &mut Vec::new(), db::now())
            .await
            .unwrap();
    }
    assert_eq!(
        audit_rows(&core.db).await,
        [
            ("probes_listed".to_owned(), false, None, json!({ "probes": 1 })),
            (
                "probe_quarantined".to_owned(),
                false,
                Some(delta()),
                json!({ "by": "admin", "reason": "reads more than agreed", "already": false })
            ),
            (
                "probe_released".to_owned(),
                false,
                Some(delta()),
                json!({ "already": false })
            ),
        ]
    );
    let verified = mercury_core::audit::verify(&core.db).await.unwrap();
    assert_eq!(verified.broken, None);
}

// ---------------------------------------------------------------- the project list

/// The caller's project list, as `GET /v1/projects` gives it.
async fn project_list_of(core: &Core, token: &str) -> Value {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let questions = mercury_core::questions::Questions::new(
        core.db.clone(),
        Arc::new(config),
        Arc::new(mercury_core::questions::NotBuilt),
    );
    let request = Request::builder()
        .uri("/v1/projects")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = mercury_core::app(questions).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// What the list says of delta's probe: its status and the time it was last heard.
fn delta_probe(list: &Value) -> &Value {
    let delta = list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["project"] == "delta")
        .unwrap_or_else(|| panic!("delta isn't listed: {list}"));
    &delta["probe"]
}

/// What a person reads of a probe's health before asking: a probe that stopped shows as `stale` in
/// the project list three minutes after its last registration, with the time it was last heard as
/// its `as_of`; one quarantined says so, and a project whose probe never registered isn't listed,
/// for it has no manifest.
#[tokio::test]
async fn a_probe_that_stopped_is_stale_in_the_project_list_with_its_last_as_of() {
    let core = core("http://127.0.0.1:9/mcp", AccessMode::Code, Duration::from_secs(1)).await;
    let now = db::now();
    admin_add(&core, "pat@example.com").await;
    let token = admin_token(&core, "pat@example.com").await;

    // never heard from, and no manifest of the admin's own: nothing to ask about yet
    let list = project_list_of(&core, &token).await;
    assert_eq!(list, json!({ "projects": [] }));

    // heard from two and a half minutes ago: healthy, and its as_of is that time
    let heard = now - TimeDelta::seconds(150);
    core.probes.record(&registration(None), heard).await.unwrap();
    sqlx::query("UPDATE probes SET status = 'healthy' WHERE project = 'delta'")
        .execute(core.db.pool())
        .await
        .unwrap();
    let list = project_list_of(&core, &token).await;
    let probe = delta_probe(&list);
    assert_eq!(probe["status"], "healthy", "{list}");
    assert_eq!(
        db::parse_time(probe["as_of"].as_str().unwrap()),
        db::parse_time(&db::time(heard)),
        "its last registration, as the database keeps it"
    );

    // silent for four minutes: stale, with the same as_of, though its row still says healthy
    let heard = now - TimeDelta::minutes(4);
    core.probes.record(&registration(None), heard).await.unwrap();
    sqlx::query("UPDATE probes SET status = 'healthy' WHERE project = 'delta'")
        .execute(core.db.pool())
        .await
        .unwrap();
    let list = project_list_of(&core, &token).await;
    let probe = delta_probe(&list);
    assert_eq!(probe["status"], "stale", "{list}");
    assert_eq!(
        db::parse_time(probe["as_of"].as_str().unwrap()),
        db::parse_time(&db::time(heard))
    );

    // quarantined stays quarantined, however long ago
    sqlx::query("UPDATE probes SET status = 'quarantined' WHERE project = 'delta'")
        .execute(core.db.pool())
        .await
        .unwrap();
    let list = project_list_of(&core, &token).await;
    assert_eq!(delta_probe(&list)["status"], "quarantined", "{list}");
}

async fn admin_add(core: &Core, email: &str) {
    let now = db::now();
    mercury_core::admin::add_user(&core.db, email, "Pat Example", mercury_schema::Role::Pm, now)
        .await
        .unwrap();
    mercury_core::admin::grant(
        &core.db,
        email,
        &["delta".to_owned()],
        false,
        &mercury_core::admin::KnownProjects::new(["delta"]),
        now,
    )
    .await
    .unwrap();
}

async fn admin_token(core: &Core, email: &str) -> String {
    mercury_core::admin::create_token(&core.db, email, "laptop", TimeDelta::days(30), db::now())
        .await
        .unwrap()
        .token
        .reveal()
}

// ---------------------------------------------------------------- what each grant lets a person ask

/// `mctl access list` with these arguments, against the configuration in `dir`.
async fn access_list(core: &Core, dir: &Path, args: &[&str]) -> String {
    let command = <mercury_core::cli::Cli as clap::Parser>::try_parse_from(
        ["mercury-core", "admin", "access", "list"]
            .iter()
            .chain(args.iter()),
    )
    .unwrap();
    let Some(mercury_core::cli::Command::Admin(command)) = command.command else {
        panic!("not an admin command");
    };
    let (mut out, mut note) = (Vec::new(), Vec::new());
    mercury_core::admin::run(command, &core.db, dir, &mut out, &mut note)
        .await
        .unwrap();
    String::from_utf8(out).unwrap()
}

/// A person with a role and grants, as the admin commands make them.
async fn person_with(core: &Core, email: &str, role: mercury_schema::Role, projects: &[&str]) {
    let now = db::now();
    mercury_core::admin::add_user(&core.db, email, "Pat Example", role, now)
        .await
        .unwrap();
    let projects: Vec<String> = projects.iter().map(|p| (*p).to_owned()).collect();
    mercury_core::admin::grant(
        &core.db,
        email,
        &projects,
        false,
        &mercury_core::admin::KnownProjects::new(["delta", "gamma"]),
        now,
    )
    .await
    .unwrap();
}

/// The words of the line of the classes section for `email` on `project`.
fn classes_of(shown: &str, email: &str, project: &str) -> String {
    let section = shown
        .split("WHAT EACH GRANT LETS THEM ASK")
        .nth(1)
        .unwrap_or_else(|| panic!("no classes section in\n{shown}"));
    let line = section
        .lines()
        .find(|line| line.starts_with(email) && line.split_whitespace().nth(1) == Some(project))
        .unwrap_or_else(|| panic!("no {email} on {project} in\n{section}"));
    line.split_whitespace().skip(2).collect::<Vec<_>>().join(" ")
}

/// `access list` says what each grant lets its person ask: the role's classes in `policies.yaml`
/// with the project's manifest, a class held for sign-off marked, a project nobody has registered a
/// manifest for named as not askable yet, and files that can't be read said so without hiding the
/// people.
#[tokio::test]
async fn the_access_listing_says_what_each_grant_lets_a_person_ask() {
    let core = core("http://127.0.0.1:9/mcp", AccessMode::Code, Duration::from_secs(1)).await;
    core.probes.record(&registration(None), db::now()).await.unwrap();
    person_with(
        &core,
        "pat@example.com",
        mercury_schema::Role::Pm,
        &["delta", "gamma"],
    )
    .await;
    person_with(
        &core,
        "tom@example.com",
        mercury_schema::Role::TechLead,
        &["delta"],
    )
    .await;
    person_with(
        &core,
        "bea@example.com",
        mercury_schema::Role::Board,
        &["delta", "portfolio"],
    )
    .await;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");

    let shown = access_list(&core, &examples, &[]).await;
    // the people table comes first, as it was, then the classes
    assert!(shown.find("PERSON  ").unwrap() < shown.find("WHAT EACH GRANT LETS THEM ASK").unwrap());
    let pm = classes_of(&shown, "pat@example.com", "delta");
    let tech_lead = classes_of(&shown, "tom@example.com", "delta");
    let board = classes_of(&shown, "bea@example.com", "delta");
    assert_eq!(
        board, "health",
        "the board reads a project's health and nothing else of it"
    );
    assert_eq!(
        classes_of(&shown, "bea@example.com", "portfolio"),
        "portfolio (held for sign-off)",
        "and across projects, where the examples hold its answers for an approver"
    );
    for class in ["progress", "tested", "blocked", "health", "changes"] {
        assert!(pm.contains(class), "{class} in {pm}");
        assert!(tech_lead.contains(class), "{class} in {tech_lead}");
    }
    assert!(!pm.contains("portfolio"), "{pm}");
    assert_eq!(
        classes_of(&shown, "pat@example.com", "gamma"),
        "nothing yet: the project has no manifest",
        "no probe and no copy of a manifest: it can't be asked"
    );

    // one project: only its grants
    let one = access_list(&core, &examples, &["--project", "delta"]).await;
    assert!(one.contains("PROJECT  delta"), "{one}");
    assert!(one.contains("bea@example.com"), "{one}");
    assert!(!one.contains("gamma") && !one.contains("portfolio"), "{one}");
    assert_eq!(classes_of(&one, "tom@example.com", "delta"), tech_lead);
    // one person: only theirs
    let person = access_list(&core, &examples, &["--email", "bea@example.com"]).await;
    assert!(
        !person.contains("pat@example.com") && !person.contains("tom@example.com"),
        "{person}"
    );

    // the demo's policy holds `health` for a project manager's sign-off: the listing says so
    let demo = examples.join("presets/demo");
    let held = access_list(&core, &demo, &["--email", "pat@example.com"]).await;
    assert!(
        classes_of(&held, "pat@example.com", "delta").contains("health (held for sign-off)"),
        "{held}"
    );

    // files that can't be read: the people are still listed, and the listing says why
    let empty = tempfile::tempdir().unwrap();
    let blind = access_list(&core, empty.path(), &[]).await;
    assert!(
        blind.contains("pat@example.com") && blind.contains("bea@example.com"),
        "{blind}"
    );
    assert!(
        blind.contains("isn't shown: projects.yaml: the file is missing"),
        "{blind}"
    );
    assert!(!blind.contains("WHAT EACH GRANT LETS THEM ASK"), "{blind}");
}

/// The project list gives each role its own classes, from the policy and the project's manifest: a
/// project manager the six a project answers (the five and `open`), a board member `health` alone, and the project only
/// to someone granted it.
#[tokio::test]
async fn the_project_list_gives_each_role_its_own_classes() {
    let core = core("http://127.0.0.1:9/mcp", AccessMode::Code, Duration::from_secs(1)).await;
    core.probes.record(&registration(None), db::now()).await.unwrap();
    person_with(&core, "pat@example.com", mercury_schema::Role::Pm, &["delta"]).await;
    person_with(&core, "bea@example.com", mercury_schema::Role::Board, &["delta"]).await;
    person_with(&core, "sam@example.com", mercury_schema::Role::Pm, &["gamma"]).await;

    let classes_for = |list: &Value| -> Vec<String> {
        let delta = list["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["project"] == "delta")
            .unwrap_or_else(|| panic!("delta isn't listed: {list}"));
        delta["classes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|class| class.as_str().unwrap().to_owned())
            .collect()
    };
    let pm = project_list_of(&core, &admin_token(&core, "pat@example.com").await).await;
    assert_eq!(
        classes_for(&pm),
        ["progress", "tested", "blocked", "health", "changes", "open"]
    );
    assert_eq!(
        pm["projects"].as_array().unwrap().len(),
        1,
        "nothing about other projects"
    );
    let board = project_list_of(&core, &admin_token(&core, "bea@example.com").await).await;
    assert_eq!(classes_for(&board), ["health"]);
    // granted another project only: delta isn't theirs to see
    let sam = project_list_of(&core, &admin_token(&core, "sam@example.com").await).await;
    assert_eq!(sam, json!({ "projects": [] }));
}
