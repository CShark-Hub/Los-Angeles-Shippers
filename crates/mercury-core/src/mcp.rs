//! MCP at `/mcp`: AI agents (Claude Code) ask through the same gateway as a person. Two tools act
//! as the token's owner: `list_projects`, the body of `GET /v1/projects`, and `ask`, which starts
//! its question through [`questions::start`] exactly as `POST /v1/questions` does (the same rate
//! limit, budget, orchestrator, checks, answer to a clarify and audit, with `channel: mcp`), waits
//! for its end, at most [`ASK_DEADLINE`] and only while the client waits, and returns where it
//! stands, built from its audited events only. The question runs on its own: whatever ends the
//! wait, it goes on; a retry with the same idempotency key gets it back instead of a second one.
//! There is no trusted-agent path: what an agent passes on is checked like what a person types.
//!
//! The transport is MCP's Streamable HTTP, stateless: no `Mcp-Session-Id`, and every `POST`
//! authenticated on its own, by its Bearer token alone. The web client's session cookie counts for
//! nothing here, so a page that holds one can't ask in its name; an `Origin`, when sent, must be
//! `MERCURY_PUBLIC_ORIGIN`. A `POST` answers as an event stream (progress, then the result), or as
//! one JSON body to a client that accepts only JSON; a request the transport refuses gets a
//! JSON-RPC error. Nothing of a question, its facts or its answer is logged.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mercury_schema::api::{
    Answer, ClarifyAnswer, ErrorCode, NewQuestion, ProjectList, QuestionState, QuestionStatus,
};
use mercury_schema::events::{Clarify, EventError};
use mercury_schema::facts::Fact;
use mercury_schema::{ProjectId, QuestionId};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, InitializeResult,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam, ProgressToken,
    ProtocolVersion, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::{self, Caller, Channel};
use crate::limits;
use crate::questions::{self, Questions, Row};
use crate::session::Sessions;

/// How long `ask` waits for its question's end: the CLI's default deadline. A question that takes
/// longer goes on, and the result says how to follow it.
pub const ASK_DEADLINE: Duration = Duration::from_secs(120);

/// The shape of a question's id, as `mercury-schema` checks it, for the tool's input schema.
const QUESTION_ID: &str = "^q_[0-9A-HJKMNP-TV-Z]{26}$";

/// What MCP asks a client to accept on every `POST`, and what the transport checks for.
const BOTH: &str = "application/json, text/event-stream";
const JSON: &str = "application/json";
const EVENT_STREAM: &str = "text/event-stream";

/// The shape of an idempotency key's characters, for the tool's input schema: printable ASCII,
/// no space, as [`is_idempotency_key`] checks it.
const KEY_CHARACTERS: &str = "^[!-~]+$";

/// JSON-RPC's codes for a request refused before it is read: a body that isn't JSON, one that
/// isn't a request this server takes, and the server's own failure.
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const INTERNAL_ERROR: i32 = -32603;

/// The newest MCP revision the server speaks; a client that proposes a later one is answered with it.
const NEWEST_PROTOCOL: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// What an agent is told when it connects.
const INSTRUCTIONS: &str = "Mercury answers questions about a project: its progress, tests, \
    blockers, health, recent changes and anything else about the work, from facts its probe gathers \
    and, where the project has a code reader, short findings from it. It never shows code. Call \
    list_projects to see which projects you may ask about, then ask. A refusal is an answer too: \
    pass its message and hint on as they are.";

const LIST_PROJECTS: &str = "The projects this token may ask about: each one's id, name and modules, \
    the kinds of question it answers, and how fresh its data is.";

const ASK: &str = "Asks Mercury a question about a project (its progress, tests, blockers, health, \
    recent changes or anything else about the work), and waits for its end, up to two minutes: the answer with the facts it cites and \
    its confidence; a refusal with its code, its reason and a hint; a question back, with its \
    options and how to answer it (ask again with clarify); or a note that an approver must release \
    the answer first. A question that takes longer goes on, and the result says how to follow it.";

/// `POST /mcp`, the core's MCP server; any other method is `405`. Mounted beside `/v1`, without
/// its session layer: the cookie never reaches the tools.
pub fn routes(questions: Questions) -> Router {
    routes_with_deadline(questions, ASK_DEADLINE)
}

/// [`routes`], with `ask` waiting at most `deadline` for its question's end.
pub fn routes_with_deadline(questions: Questions, deadline: Duration) -> Router {
    let sessions = questions.sessions.clone();
    let db = questions.db.clone();
    let transports = Transports {
        stream: transport(&questions, false, deadline),
        json: transport(&questions, true, deadline),
    };
    // the layers run only on a `POST`: another method is `405` before any of them
    let mcp = post(serve)
        .route_layer(middleware::from_fn(json_rpc_refusals))
        .route_layer(middleware::from_fn_with_state(db, bearer_only))
        .route_layer(middleware::from_fn(limits::body_cap))
        .route_layer(middleware::from_fn_with_state(sessions, same_origin));
    Router::new().route("/mcp", mcp).with_state(transports)
}

/// The two ways a `POST` is answered.
#[derive(Clone)]
struct Transports {
    /// An event stream: progress notifications, then the result.
    stream: StreamableHttpService<Server, NeverSessionManager>,
    /// One JSON body, for a client that accepts only JSON.
    json: StreamableHttpService<Server, NeverSessionManager>,
}

fn transport(
    questions: &Questions,
    json: bool,
    deadline: Duration,
) -> StreamableHttpService<Server, NeverSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(json)
        .with_max_request_body_bytes(limits::MAX_BODY)
        // behind Caddy the core sees whatever host the edge forwards; the Bearer token and the
        // Origin rule guard the route, not the name it was reached by
        .disable_allowed_hosts();
    let questions = questions.clone();
    StreamableHttpService::new(
        move || {
            Ok(Server {
                questions: questions.clone(),
                deadline,
            })
        },
        Arc::new(NeverSessionManager::default()),
        config,
    )
}

/// Marks a request whose client accepts only JSON: its answer is one body, so nothing may come
/// before it, no progress either.
#[derive(Debug, Clone, Copy)]
struct JsonOnly;

/// Hands a `POST` to the transport its `Accept` asks for.
async fn serve(State(transports): State<Transports>, mut request: Request) -> Response {
    let json_only = request
        .headers()
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains(JSON) && !accept.contains(EVENT_STREAM));
    let transport = if json_only {
        // the transport wants both named; this client gets one JSON body and no progress
        request
            .headers_mut()
            .insert(ACCEPT, HeaderValue::from_static(BOTH));
        request.extensions_mut().insert(JsonOnly);
        &transports.json
    } else {
        &transports.stream
    };
    transport.handle(request).await.map(Body::new)
}

/// Gives a request the transport refuses before any tool runs a JSON-RPC error body. The
/// transport answers a wrong `Accept` or content type, a body that isn't JSON or isn't JSON-RPC, a
/// protocol version it doesn't know or a missing `Host` with a line of plain text, some of which
/// repeats what was sent. The status stays; the body becomes an error with no id, which says what
/// was wrong in words of its own and nothing of the request. What the transport answers in JSON,
/// its own JSON-RPC errors among them, passes as it is.
async fn json_rpc_refusals(request: Request, next: Next) -> Response {
    let json_body = is_json(request.headers().get(CONTENT_TYPE));
    let (parts, body) = request.into_parts();
    // the body cap read the body whole: this takes it back as it is, to tell later why it was
    // refused
    let Ok(sent) = axum::body::to_bytes(body, limits::MAX_BODY).await else {
        return json_rpc_error(
            StatusCode::BAD_REQUEST,
            INVALID_REQUEST,
            "The request body can't be read.",
        );
    };
    let response = next
        .run(Request::from_parts(parts, Body::from(sent.clone())))
        .await;
    let status = response.status();
    let refused = status.is_client_error() || status.is_server_error();
    if !refused || is_json(response.headers().get(CONTENT_TYPE)) {
        return response;
    }
    let (code, message) = match status {
        StatusCode::NOT_ACCEPTABLE => (
            INVALID_REQUEST,
            "The request must accept application/json, and text/event-stream for progress.",
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE if !json_body => (
            INVALID_REQUEST,
            "The request's Content-Type must be application/json.",
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE if serde_json::from_slice::<IgnoredAny>(&sent).is_err() => {
            (PARSE_ERROR, "The request body isn't JSON.")
        }
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (
            INVALID_REQUEST,
            "The request body isn't a JSON-RPC message this server takes.",
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (INVALID_REQUEST, "The request body is too large."),
        StatusCode::BAD_REQUEST => (
            INVALID_REQUEST,
            "The request's Host or MCP-Protocol-Version header isn't one this server takes.",
        ),
        status if status.is_server_error() => (INTERNAL_ERROR, "The server could not take the request."),
        _ => (INVALID_REQUEST, "The request isn't one this server takes."),
    };
    json_rpc_error(status, code, message)
}

/// Whether a `Content-Type` is JSON, as the transport tells it.
fn is_json(content_type: Option<&HeaderValue>) -> bool {
    content_type
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with(JSON))
}

/// A JSON-RPC error for a request whose id was never read, with `status`.
fn json_rpc_error(status: StatusCode, code: i32, message: &str) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": code, "message": message } });
    (
        status,
        [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
        body.to_string(),
    )
        .into_response()
}

/// An `Origin`, when a request carries one, must be Mercury's own page: MCP's rule against DNS
/// rebinding. Agents send none; with no `MERCURY_PUBLIC_ORIGIN` set, no `Origin` can pass.
async fn same_origin(State(sessions): State<Sessions>, request: Request, next: Next) -> Response {
    if let Some(sent) = request.headers().get(ORIGIN) {
        let expected = sessions.config().public_origin.as_deref();
        if expected.is_none_or(|expected| sent.to_str().ok() != Some(expected)) {
            return ApiError::foreign_origin().into_response();
        }
    }
    next.run(request).await
}

/// Authenticates every request by its `Authorization: Bearer` token alone, checked as anywhere
/// else; the session cookie is never read here, valid or not. The caller then rides in the
/// request's extensions, where the tools find it.
async fn bearer_only(State(db): State<Db>, mut request: Request, next: Next) -> Response {
    match identity::authenticate(&db, request.headers().get(AUTHORIZATION), db::now()).await {
        Ok(caller) => {
            request.extensions_mut().insert(caller);
            next.run(request).await
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------------------------
// The tools

/// The arguments of `ask`; any other is refused. `clarify` and `follows` are what
/// `POST /v1/questions` takes, and [`questions::start`] checks them as it does there;
/// `idempotency_key` is that route's `Idempotency-Key`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskArguments {
    question: String,
    #[serde(default)]
    project: Option<ProjectId>,
    #[serde(default)]
    clarify: Option<ClarifyAnswer>,
    #[serde(default)]
    follows: Option<QuestionId>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

/// Reads and checks the arguments of an `ask` call, before the call costs anything: their shape,
/// the question's length as [`questions::start`] checks it, and the key's, as `POST /v1/questions`
/// checks its header. A key that isn't one is never repeated.
fn ask_arguments(arguments: JsonObject) -> Result<AskArguments, ApiError> {
    let Ok(arguments) = serde_json::from_value::<AskArguments>(Value::Object(arguments)) else {
        return Err(questions::invalid(
            "The arguments aren't the ones ask takes.",
            "Send question; project if you know it; clarify to answer a question back; follows \
             to ask again after a rejection; idempotency_key to make a retry the same question.",
        ));
    };
    let chars = arguments.question.trim().chars().count();
    if !(1..=questions::MAX_TEXT_CHARS).contains(&chars) {
        return Err(questions::invalid(
            "The question must be 1 to 2,000 characters.",
            "Ask a shorter question.",
        ));
    }
    if arguments
        .idempotency_key
        .as_deref()
        .is_some_and(|key| !is_idempotency_key(key))
    {
        return Err(questions::invalid(
            "The idempotency_key must be 1 to 128 printable ASCII characters, without spaces.",
            "Send a new random key with each question, and the same key when you retry it.",
        ));
    }
    Ok(arguments)
}

/// Whether `key` is an idempotency key: the rule of `POST /v1/questions`'s `Idempotency-Key`
/// header, 1 to [`questions::MAX_KEY_LEN`] printable ASCII characters, no space.
fn is_idempotency_key(key: &str) -> bool {
    (1..=questions::MAX_KEY_LEN).contains(&key.len()) && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn tools() -> Vec<Tool> {
    let ask = json!({
        "type": "object",
        "properties": {
            "question": {
                "type": "string",
                "minLength": 1,
                "maxLength": questions::MAX_TEXT_CHARS,
                "description": "The question, in the words a person would use."
            },
            "project": {
                "type": "string",
                "description": "A project id from list_projects. Left out, Mercury tells the project from the question, or asks back."
            },
            "clarify": {
                "type": "object",
                "description": "Only to answer a question back: ask the same question again, with of the id of the question that asked back and option the id of the option picked.",
                "properties": {
                    "of": {
                        "type": "string",
                        "pattern": QUESTION_ID,
                        "description": "The id of the question that ended with the question back."
                    },
                    "option": {
                        "type": "string",
                        "description": "The id of the option picked, as the question back listed it."
                    }
                },
                "required": ["of", "option"],
                "additionalProperties": false
            },
            "follows": {
                "type": "string",
                "pattern": QUESTION_ID,
                "description": "Only to ask again after an approver rejected an answer: the id of your own question that ended rejected_by_approver. The new question is linked to it in the audit."
            },
            "idempotency_key": {
                "type": "string",
                "minLength": 1,
                "maxLength": questions::MAX_KEY_LEN,
                "pattern": KEY_CHARACTERS,
                "description": "A key of your own for this question, 1 to 128 printable ASCII characters without spaces. Send the same key when you retry a call, and you get the question it started back instead of a new one. Left out, each call is a new question."
            }
        },
        "required": ["question"],
        "additionalProperties": false
    });
    let nothing = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        Tool::new("list_projects", LIST_PROJECTS, object(nothing)),
        Tool::new("ask", ASK, object(ask)),
    ]
}

fn object(schema: Value) -> JsonObject {
    match schema {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

/// The MCP server: one per request, all sharing the questions' state.
#[derive(Clone)]
struct Server {
    questions: Questions,
    /// How long `ask` waits for its question's end.
    deadline: Duration,
}

impl ServerHandler for Server {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mercury", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    /// The revisions whose results these tools build, so a newer client steps down to one of
    /// them. The library knows a later revision, whose `tools/list` must also say how long it may be
    /// cached; agreeing to it would hand that client a list it rejects, and no tool at all.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&NEWEST_PROTOCOL))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let parts = context.extensions.get::<Parts>();
        let caller = parts.and_then(|parts| parts.extensions.get::<Caller>()).cloned();
        let json_only = parts.is_some_and(|parts| parts.extensions.get::<JsonOnly>().is_some());
        let Some(caller) = caller else {
            // the layer before the transport authenticated the request: without its caller,
            // nothing runs
            tracing::error!("an MCP call came without its caller");
            return Ok(failure(&ApiError::internal()).into());
        };
        let arguments = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "list_projects" => self.list_projects(&caller, &arguments).await,
            "ask" => {
                let progress = context
                    .meta
                    .get_progress_token()
                    .filter(|_| !json_only)
                    .map(|token| Progress {
                        peer: context.peer.clone(),
                        token,
                        sent: 0,
                    });
                // the call's own token: the transport cancels it when the client goes away, its
                // connection closed before the result
                let cancelled = context.ct.clone().cancelled_owned();
                self.ask(caller, arguments, progress, cancelled).await
            }
            _ => {
                return Err(ErrorData::invalid_params(
                    "There is no such tool: Mercury has ask and list_projects.",
                    None,
                ));
            }
        };
        Ok(result.into())
    }
}

impl Server {
    /// `list_projects()`: what `GET /v1/projects` gives the token's owner.
    async fn list_projects(&self, caller: &Caller, arguments: &JsonObject) -> CallToolResult {
        if !arguments.is_empty() {
            return failure(&questions::invalid(
                "list_projects takes no arguments.",
                "Call it with no arguments.",
            ));
        }
        match questions::project_list(&self.questions, caller).await {
            Ok(list) => {
                let text = projects_text(&list);
                answered(&list, text, false)
            }
            Err(err) => failure(&err),
        }
    }

    /// `ask(question, project?, clarify?, idempotency_key?)`: starts the question as
    /// `POST /v1/questions` does, follows its audited events to its end, at most the deadline and
    /// until `cancelled`, and gives where it stands then.
    async fn ask(
        &self,
        caller: Caller,
        arguments: JsonObject,
        mut progress: Option<Progress>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> CallToolResult {
        let until = tokio::time::Instant::now() + self.deadline;
        let questions = &self.questions;
        let arguments = match ask_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(err) => return failure(&err),
        };
        // a call whose arguments are accepted spends one request of the token's own allowance,
        // the one `POST /v1/questions` spends; one refused for its arguments spends nothing
        if let Err(wait) = questions.limiter.check(&caller.token.public_id, Instant::now()) {
            return failure(&ApiError::rate_limited(wait));
        }
        // the agent's key, so that its retry gets the question the first call started; without
        // one, a key of its own for this call, which makes it a new question
        let key = match arguments.idempotency_key {
            Some(key) => key,
            None => match fresh_key() {
                Ok(key) => key,
                Err(err) => {
                    tracing::error!(error = %err, "cannot draw an idempotency key");
                    return failure(&ApiError::internal());
                }
            },
        };
        let new = NewQuestion {
            text: arguments.question,
            project: arguments.project,
            clarify: arguments.clarify,
            follows: arguments.follows,
        };
        let id = match questions::start(questions, caller, Channel::Mcp, &key, new).await {
            Ok(id) => id,
            Err(err) => return failure(&err),
        };
        // the question's events as its stream sends them, up to its end, the deadline or the
        // client's cancelling the call, whichever comes first; only the wait stops, the question
        // runs on its own, as it does without a stream
        let mut rows = questions::rows(questions, id.clone(), 0);
        let waited = wait(&mut rows, &mut progress, until, cancelled).await;
        drop(rows);
        if waited == Waited::Cancelled {
            tracing::debug!("an ask was cancelled by its client; its question goes on");
        }
        match questions::state_of(&questions.db, &id).await {
            Ok(state) => {
                let origin = questions.sessions.config().public_origin.as_deref();
                told(&questions.db, state, origin).await
            }
            Err(err) => failure(&err),
        }
    }
}

/// How a wait for a question's end stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    /// Its events reached their end.
    Ended,
    /// The deadline came first.
    Deadline,
    /// The client cancelled the call.
    Cancelled,
}

/// Follows a question's `rows` to their end, telling `progress` of each event before it, until
/// `until` or until `cancelled` resolves, whichever comes first.
async fn wait(
    rows: &mut mpsc::Receiver<Row>,
    progress: &mut Option<Progress>,
    until: tokio::time::Instant,
    cancelled: impl Future<Output = ()>,
) -> Waited {
    let deadline = tokio::time::sleep_until(until);
    tokio::pin!(deadline, cancelled);
    loop {
        tokio::select! {
            // a client that went away needs nothing more, and an end that came beats the deadline
            biased;
            () = &mut cancelled => return Waited::Cancelled,
            row = rows.recv() => {
                let Some(row) = row else { return Waited::Ended };
                if let Some(progress) = progress.as_mut()
                    && !row.is_terminal()
                    && row.kind != "answer"
                {
                    progress.event(&row.kind).await;
                }
            }
            () = &mut deadline => return Waited::Deadline,
        }
    }
}

/// Where `ask` reports its question's progress: the client's token, and the events so far.
struct Progress {
    peer: Peer<RoleServer>,
    token: ProgressToken,
    sent: u32,
}

impl Progress {
    /// One notification: the count so far and the event's name, never anything of its data.
    async fn event(&mut self, name: &str) {
        self.sent += 1;
        let params =
            ProgressNotificationParam::new(self.token.clone(), f64::from(self.sent)).with_message(name);
        if let Err(err) = self.peer.notify_progress(params).await {
            tracing::debug!(error = %err, "a progress notification can't be sent");
        }
    }
}

/// A key for one question, in the shape `POST /v1/questions` takes.
fn fresh_key() -> Result<String, getrandom::Error> {
    let mut random = [0_u8; 18];
    getrandom::fill(&mut random)?;
    Ok(format!("mcp-{}", URL_SAFE_NO_PAD.encode(random)))
}

/// The result of `ask` once its wait has stopped: `structuredContent` the body of
/// `GET /v1/questions/{id}` at that moment, `content` one text to relay. Only a failure is a tool
/// error: a refusal is an answer, and so is a question still running. `origin` is the web
/// client's, where a question still running can be followed.
async fn told(db: &Db, state: QuestionState, origin: Option<&str>) -> CallToolResult {
    let id = state.id.as_str();
    let text = match state.status {
        QuestionStatus::Delivered => state.answer.as_ref().map(answer_text),
        QuestionStatus::Refused => state.refusal.as_ref().map(|refusal| {
            format!(
                "Refused ({}): {}\n{}",
                name(&refusal.code),
                refusal.message,
                refusal.hint
            )
        }),
        QuestionStatus::Clarify => match questions::last_event::<Clarify>(db, id, "clarify").await {
            Ok(clarify) => clarify.as_ref().map(|clarify| clarify_text(id, clarify)),
            Err(err) => return failure(&err),
        },
        QuestionStatus::PendingSignoff => Some(format!(
            "Held for sign-off: an approver must release the answer before it is shown. Question {id}."
        )),
        QuestionStatus::Failed => match questions::last_event::<EventError>(db, id, "error").await {
            Ok(Some(error)) => Some(format!("Failed ({}): {}", name(&error.code), error.message)),
            Ok(None) => Some("Failed (internal): The server could not finish the question.".to_owned()),
            Err(err) => return failure(&err),
        },
        // the deadline or the client stopped the wait first, or the events couldn't be read
        QuestionStatus::Running => Some(still_running(id, origin)),
    };
    let Some(text) = text else {
        tracing::error!("a question that ended can't be told");
        return failure(&ApiError::internal());
    };
    answered(&state, text, state.status == QuestionStatus::Failed)
}

/// A question that hadn't ended when the wait stopped: its id, that it goes on, and how to follow
/// it, since another `ask` without the same `idempotency_key` would start another question.
fn still_running(id: &str, origin: Option<&str>) -> String {
    format!(
        "Still running: question {id} hasn't ended yet, and it goes on without this call. Asking \
         again without the same idempotency_key would start another question. Follow it in the web \
         client at {}/ask/{id}, or with \
         `mercury ask --wait {id}`.",
        origin.unwrap_or_default()
    )
}

/// A tool result: `structured` as `structuredContent`, `text` as its one content item.
fn answered<T: Serialize>(structured: &T, text: String, is_error: bool) -> CallToolResult {
    let Ok(structured) = serde_json::to_value(structured) else {
        return failure(&ApiError::internal());
    };
    let mut result = CallToolResult::structured(structured);
    result.content = vec![ContentBlock::text(text)];
    result.is_error = Some(is_error);
    result
}

/// A question that couldn't start, or a list that couldn't be made, as a tool error: the API's
/// error body, with the seconds to wait in the message for the two `429` codes, since a tool
/// result has no `Retry-After`.
fn failure(error: &ApiError) -> CallToolResult {
    let mut body = error.body();
    if let Some(seconds) = error.retry_after {
        body.error.message = format!("{} Retry in {seconds} seconds.", body.error.message);
    }
    let detail = &body.error;
    let text = format!(
        "{} ({}): {}\n{}",
        if detail.code == ErrorCode::Internal {
            "Failed"
        } else {
            "Not asked"
        },
        name(&detail.code),
        detail.message,
        detail.hint.as_deref().unwrap_or_default()
    );
    let mut result = CallToolResult::structured_error(serde_json::to_value(&body).unwrap_or(Value::Null));
    result.content = vec![ContentBlock::text(text)];
    result
}

/// The answer, the facts it cites, one a line, and its confidence.
fn answer_text(answer: &Answer) -> String {
    let mut lines = vec![answer.text.clone()];
    if !answer.facts.is_empty() {
        lines.push(String::new());
        lines.push(format!("Facts ({}):", answer.facts.len()));
        lines.extend(answer.facts.iter().map(|fact| format!("- {}", fact_text(fact))));
    }
    lines.push(String::new());
    lines.push(format!("Confidence: {:.2}", answer.confidence.get()));
    lines.join("\n")
}

fn fact_text(fact: &Fact) -> String {
    let mut parts = vec![
        fact.title.as_str().to_owned(),
        fact.module.as_str().to_owned(),
        words(&fact.status),
        format!("tests {}", words(&fact.tested)),
    ];
    if let Some(waiting) = &fact.waiting_on {
        parts.push(format!("waiting on {}", words(waiting)));
    }
    parts.push(format!("updated {}", fact.updated_at.format("%Y-%m-%d")));
    parts.join(" · ")
}

/// The question back, each option's id and label, and how to answer it over MCP: `ask` again with
/// the same question and `clarify`, for a question back about the project and one about the kind
/// of question alike.
fn clarify_text(id: &str, clarify: &Clarify) -> String {
    let mut lines = vec![clarify.question.clone()];
    lines.extend(
        clarify
            .options
            .iter()
            .map(|option| format!("- {}: {}", option.id, option.label)),
    );
    lines.push(format!(
        "To answer, call ask again with the same question and clarify set to \
         {{\"of\": \"{id}\", \"option\": \"<the id of the option picked>\"}}."
    ));
    lines.join("\n")
}

/// One line per project.
fn projects_text(list: &ProjectList) -> String {
    if list.projects.is_empty() {
        return "No projects yet: ask the admin for access.".to_owned();
    }
    let lines: Vec<String> = list
        .projects
        .iter()
        .map(|project| {
            let classes: Vec<String> = project.classes.iter().map(name).collect();
            let probe = match &project.probe {
                Some(probe) => format!(
                    "{}, data as of {}",
                    words(&probe.status),
                    probe.as_of.format("%Y-%m-%d %H:%M UTC")
                ),
                None => "not registered yet".to_owned(),
            };
            format!(
                "{}: {}. Classes: {}. Probe: {probe}.",
                project.project,
                project.display_name,
                if classes.is_empty() {
                    "none".to_owned()
                } else {
                    classes.join(", ")
                }
            )
        })
        .collect();
    lines.join("\n")
}

/// A value's name on the wire, `asks_for_code`.
fn name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => "unknown".to_owned(),
    }
}

/// A value's name in words, `in review`.
fn words<T: Serialize>(value: &T) -> String {
    name(value).replace('_', " ")
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    #[test]
    fn two_tools_whose_schemas_refuse_any_other_argument() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, ["list_projects", "ask"]);
        for tool in &tools {
            assert_eq!(tool.input_schema["additionalProperties"], false, "{}", tool.name);
        }
        let ask = &tools[1].input_schema;
        assert_eq!(ask["required"], json!(["question"]));
        assert_eq!(ask["properties"]["question"]["maxLength"], 2000);
        let clarify = &ask["properties"]["clarify"];
        assert_eq!(clarify["required"], json!(["of", "option"]));
        assert_eq!(clarify["additionalProperties"], false);
        let key = &ask["properties"]["idempotency_key"];
        assert_eq!((&key["minLength"], &key["maxLength"]), (&json!(1), &json!(128)));
        assert_eq!(ask["properties"]["follows"]["pattern"], QUESTION_ID);
    }

    /// The input schema's key is the one `ask` takes: the header's rule on `POST /v1/questions`.
    #[test]
    fn the_schemas_key_is_the_one_ask_takes() {
        let pattern = regex::Regex::new(KEY_CHARACTERS).unwrap();
        let longest = "~".repeat(questions::MAX_KEY_LEN);
        for key in [
            "k",
            "agent-call-0001",
            "!~",
            longest.as_str(),
            "",
            " ",
            "a b",
            "tab\t",
            "\u{e9}",
        ] {
            assert_eq!(pattern.is_match(key), is_idempotency_key(key), "{key:?}");
        }
        assert!(is_idempotency_key(&longest));
        assert!(!is_idempotency_key(&format!("{longest}~")), "one too long");
    }

    #[test]
    fn a_key_that_isnt_one_is_refused_without_being_repeated() {
        let arguments = |value: Value| match value {
            Value::Object(map) => ask_arguments(map),
            _ => unreachable!(),
        };
        let kept = arguments(json!({ "question": "What is done?", "idempotency_key": "agent-1" }))
            .ok()
            .unwrap();
        assert_eq!(kept.idempotency_key.as_deref(), Some("agent-1"));
        let none = arguments(json!({ "question": "What is done?", "idempotency_key": null })).ok();
        assert!(none.unwrap().idempotency_key.is_none());
        for key in ["", "secret key", "secret\u{e9}"] {
            let Err(error) = arguments(json!({ "question": "What is done?", "idempotency_key": key })) else {
                panic!("{key:?} was taken");
            };
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert!(
                !error.message.contains("secret") && !error.hint.contains("secret"),
                "{key:?}"
            );
        }
    }

    /// The input schema's pattern for a question's id is the one `mercury-schema` checks.
    #[test]
    fn the_schemas_question_id_is_the_schema_crates() {
        let pattern = regex::Regex::new(QUESTION_ID).unwrap();
        for id in [
            "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
            "q_01jb2m3n4p5q6r7s8t9v0w1x2y",
            "q_01JB2M3N4P5Q6R7S8T9V0W1X2",
            "q_01JB2M3N4P5Q6R7S8T9V0W1X2YZ",
            "u_01JB2M3N4P5Q6R7S8T9V0W1X2Y",
            "q_01JB2M3N4P5Q6R7S8T9V0W1XIL",
        ] {
            let parsed = mercury_schema::QuestionId::parse(id).is_ok();
            assert_eq!(pattern.is_match(id), parsed, "{id}");
        }
    }

    #[test]
    fn ask_takes_a_question_a_project_and_a_clarify_and_nothing_else() {
        let parse = |value: Value| serde_json::from_value::<AskArguments>(value).ok();
        let both = parse(json!({ "question": "What is done?", "project": "delta" })).unwrap();
        assert_eq!(both.project.unwrap().as_str(), "delta");
        assert!(both.clarify.is_none());
        assert!(parse(json!({ "question": "What is done?", "project": null })).is_some());
        let of = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y";
        let answer =
            parse(json!({ "question": "What is done?", "clarify": { "of": of, "option": "kappa" } }))
                .unwrap()
                .clarify
                .unwrap();
        assert_eq!((answer.of.as_str(), answer.option.as_str()), (of, "kappa"));
        let again = parse(json!({ "question": "What is done?", "follows": of })).unwrap();
        assert_eq!(again.follows.unwrap().as_str(), of);
        assert!(parse(json!({ "question": "What is done?", "follows": "q_x" })).is_none());
        for clarify in [
            json!({}),
            json!({ "of": of }),
            json!({ "of": "q_x", "option": "kappa" }),
            json!({ "of": of, "option": "kappa", "trusted": true }),
        ] {
            let arguments = json!({ "question": "What is done?", "clarify": clarify });
            assert!(parse(arguments.clone()).is_none(), "{arguments}");
        }
        assert!(parse(json!({ "project": "delta" })).is_none());
        assert!(parse(json!({ "question": 7 })).is_none());
        assert!(parse(json!({ "question": "What?", "project": "Not a project" })).is_none());
    }

    fn row(seq: u64, kind: &str) -> Row {
        Row {
            seq,
            kind: kind.to_owned(),
            payload: None,
        }
    }

    fn after(millis: u64) -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_millis(millis)
    }

    #[tokio::test]
    async fn the_wait_ends_with_the_questions_events() {
        let (tx, mut rows) = mpsc::channel(4);
        tx.send(row(1, "accepted")).await.unwrap();
        tx.send(row(2, "refused")).await.unwrap();
        drop(tx);
        let waited = wait(&mut rows, &mut None, after(60_000), std::future::pending()).await;
        assert_eq!(waited, Waited::Ended);
    }

    #[tokio::test]
    async fn the_wait_stops_at_the_deadline_while_the_question_runs() {
        let (tx, mut rows) = mpsc::channel(4);
        tx.send(row(1, "accepted")).await.unwrap();
        let waited = wait(&mut rows, &mut None, after(50), std::future::pending()).await;
        assert_eq!(waited, Waited::Deadline);
        assert!(!tx.is_closed(), "the wait was still following the question");
    }

    #[tokio::test]
    async fn the_wait_stops_when_the_client_cancels_the_call() {
        let (tx, mut rows) = mpsc::channel(4);
        tx.send(row(1, "accepted")).await.unwrap();
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let waiting = tokio::spawn(async move {
            let cancelled = async {
                let _ = cancelled.await;
            };
            wait(&mut rows, &mut None, after(60_000), cancelled).await
        });
        cancel.send(()).unwrap();
        let waited = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait stops at once")
            .unwrap();
        assert_eq!(waited, Waited::Cancelled);
        drop(tx);
    }

    #[test]
    fn a_question_still_running_says_how_to_follow_it() {
        let id = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y";
        let said = still_running(id, Some("https://mercury.example.com"));
        assert!(said.starts_with("Still running: question q_01JB"), "{said}");
        assert!(
            said.contains("https://mercury.example.com/ask/q_01JB2M3N4P5Q6R7S8T9V0W1X2Y"),
            "{said}"
        );
        assert!(
            said.contains("`mercury ask --wait q_01JB2M3N4P5Q6R7S8T9V0W1X2Y`"),
            "{said}"
        );
        assert!(said.contains("without the same idempotency_key"), "{said}");
        assert!(still_running(id, None).contains(" at /ask/q_01JB"));
    }

    #[test]
    fn a_wait_is_in_the_message_of_a_limited_call() {
        let result = failure(&ApiError::rate_limited(std::time::Duration::from_secs(120)));
        assert_eq!(result.is_error, Some(true));
        let body = result.structured_content.unwrap();
        assert_eq!(body["error"]["code"], "rate_limited");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("120 seconds"), "{message}");
    }

    #[test]
    fn a_fresh_key_has_the_shape_an_idempotency_key_takes() {
        let (one, two) = (fresh_key().unwrap(), fresh_key().unwrap());
        assert_ne!(one, two);
        assert!(is_idempotency_key(&one), "{one}");
    }

    #[test]
    fn a_status_code_failure_is_named() {
        let error = questions::invalid(
            "The question must be 1 to 2,000 characters.",
            "Ask a shorter question.",
        );
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        let result = serde_json::to_value(failure(&error)).unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Not asked (invalid_request):"), "{text}");
        assert_eq!(result["structuredContent"]["error"]["code"], "invalid_request");
    }
}
