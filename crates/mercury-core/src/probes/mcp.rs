//! The core's MCP client for one probe: JSON-RPC over Streamable HTTP, with the probe's token.
//! Every call has a time to answer, [`MCP_TIMEOUT`] and for the code reader's [`ASK_CODE_TIMEOUT`],
//! and reads at most [`MAX_REPLY_BYTES`] of the reply, whether the probe answers with JSON or with
//! an event stream. Errors name what went wrong, never the
//! token, the arguments or the reply.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use mercury_schema::ToolName;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};

/// How long one call to a probe may take, reading the reply included: `initialize`, `tools/list`
/// and every tool but the code reader.
pub const MCP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the code reader's `ask_code` may take: it reads the snapshot with a model, and the
/// probe's own wait for it is shorter (40 s), so a reader that is slow answers before the core gives up.
pub const ASK_CODE_TIMEOUT: Duration = Duration::from_secs(45);

/// The largest reply the core reads from a probe; a larger one breaks the contract.
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// The MCP version the core asks for.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// More pages of `tools/list` than this is not a probe of six tools.
const MAX_TOOL_PAGES: usize = 4;

const SESSION: &str = "mcp-session-id";

/// The token a probe and its core share. It never prints.
#[derive(Clone)]
pub struct ProbeSecret(Arc<str>);

impl ProbeSecret {
    /// The token without surrounding whitespace; `None` when nothing is left.
    pub fn new(token: &str) -> Option<Self> {
        let token = token.trim();
        (!token.is_empty()).then(|| Self(token.into()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProbeSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProbeSecret(..)")
    }
}

/// Why a call to a probe gave no reply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("the probe gave no reply in time")]
    Timeout,
    #[error("the request to the probe failed before a reply arrived")]
    Transport,
    #[error("the probe answered with HTTP {0}")]
    Status(u16),
    #[error("the probe's reply is over {} KB", MAX_REPLY_BYTES / 1024)]
    TooLarge,
    #[error("the probe's reply breaks the protocol: {0}")]
    Malformed(&'static str),
    #[error("the probe answered with error {0}")]
    Rpc(i64),
    #[error("the probe's tool reported a failure")]
    ToolFailed,
}

impl McpError {
    /// Whether the probe broke the contract, as opposed to being slow, down or refusing a call:
    /// a probe that does is quarantined.
    pub fn breaks_the_contract(&self) -> bool {
        matches!(self, McpError::TooLarge | McpError::Malformed(_))
    }
}

/// What `initialize` agreed on, sent with every later request.
#[derive(Debug, Clone)]
struct Session {
    protocol_version: String,
    id: Option<String>,
}

/// A probe's MCP endpoint, such as `http://probe-delta:7000/mcp`.
#[derive(Debug, Clone)]
pub struct McpClient {
    http: reqwest::Client,
    url: String,
    token: ProbeSecret,
    session: Arc<Mutex<Option<Session>>>,
    next_id: Arc<AtomicU64>,
    /// How long `initialize`, `tools/list` and every tool but the code reader may take.
    timeout: Duration,
    /// How long `ask_code` may take.
    ask_code_timeout: Duration,
}

impl McpClient {
    pub fn new(http: reqwest::Client, url: impl Into<String>, token: ProbeSecret) -> Self {
        Self {
            http,
            url: url.into(),
            token,
            session: Arc::default(),
            next_id: Arc::new(AtomicU64::new(1)),
            timeout: MCP_TIMEOUT,
            ask_code_timeout: ASK_CODE_TIMEOUT,
        }
    }

    /// The same client with other times to answer, for a test that runs them in milliseconds.
    #[must_use]
    pub fn with_timeouts(mut self, timeout: Duration, ask_code_timeout: Duration) -> Self {
        self.timeout = timeout;
        self.ask_code_timeout = ask_code_timeout;
        self
    }

    /// How long a call to this tool may take.
    pub fn timeout_for(&self, tool: ToolName) -> Duration {
        match tool {
            ToolName::AskCode => self.ask_code_timeout,
            _ => self.timeout,
        }
    }

    /// The HTTP client every probe call shares: no redirects, and the lookups' time to answer as its
    /// own backstop. Each request sets the time of its own call, which overrides this one.
    pub fn http() -> reqwest::Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(MCP_TIMEOUT)
            .redirect(Policy::none())
            .build()
    }

    /// `initialize`, then `notifications/initialized`. The probe must offer tools.
    pub async fn initialize(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "mercury-core", "version": env!("CARGO_PKG_VERSION")},
        });
        let (result, headers) = self.request("initialize", params, None, self.timeout).await?;
        let result: InitializeResult = serde_json::from_value(result)
            .map_err(|_| McpError::Malformed("initialize has no protocol version"))?;
        if result.capabilities.tools.is_none() {
            return Err(McpError::Malformed("the probe offers no tools"));
        }
        let session = Session {
            protocol_version: result.protocol_version,
            id: headers
                .get(SESSION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        };
        self.notify("notifications/initialized", &session).await?;
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
        Ok(())
    }

    /// The names of the tools the probe serves, every page of them.
    pub async fn list_tools(&self) -> Result<Vec<String>, McpError> {
        let session = self.session().await?;
        let mut names = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let (result, _) = self
                .request("tools/list", params, Some(&session), self.timeout)
                .await?;
            let page: ListToolsResult = serde_json::from_value(result)
                .map_err(|_| McpError::Malformed("tools/list has no tool list"))?;
            names.extend(page.tools.into_iter().map(|tool| tool.name));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(names),
            }
        }
        Err(McpError::Malformed("tools/list doesn't end"))
    }

    /// The structured content of a tool's reply, which the probe has `timeout` to give.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let session = self.session().await?;
        let params = json!({ "name": name, "arguments": arguments });
        let (result, _) = self
            .request("tools/call", params, Some(&session), timeout)
            .await?;
        let result: CallToolResult =
            serde_json::from_value(result).map_err(|_| McpError::Malformed("tools/call has no result"))?;
        if result.is_error.unwrap_or(false) {
            return Err(McpError::ToolFailed);
        }
        result
            .structured_content
            .ok_or(McpError::Malformed("the reply has no structured content"))
    }

    /// The session `initialize` agreed on, initializing first when there is none yet.
    async fn session(&self) -> Result<Session, McpError> {
        let known = self
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match known {
            Some(session) => Ok(session),
            None => {
                self.initialize().await?;
                let session = self
                    .session
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                session.ok_or(McpError::Malformed("initialize agreed on nothing"))
            }
        }
    }

    fn post(&self, body: &Value, session: Option<&Session>, timeout: Duration) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(&self.url)
            .timeout(timeout)
            .bearer_auth(self.token.expose())
            .header(ACCEPT, "application/json, text/event-stream")
            .json(body);
        if let Some(session) = session {
            request = request.header("mcp-protocol-version", &session.protocol_version);
            if let Some(id) = &session.id {
                request = request.header(SESSION, id);
            }
        }
        request
    }

    async fn notify(&self, method: &str, session: &Session) -> Result<(), McpError> {
        let body = json!({ "jsonrpc": "2.0", "method": method });
        let response = self
            .post(&body, Some(session), self.timeout)
            .send()
            .await
            .map_err(transport)?;
        if !response.status().is_success() {
            return Err(McpError::Status(response.status().as_u16()));
        }
        Ok(())
    }

    /// One request and the result of its response, with the response's headers.
    async fn request(
        &self,
        method: &str,
        params: Value,
        session: Option<&Session>,
        timeout: Duration,
    ) -> Result<(Value, HeaderMap), McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut response = self
            .post(&body, session, timeout)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(McpError::Status(status.as_u16()));
        }
        let headers = response.headers().clone();
        let stream = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|kind| kind.starts_with("text/event-stream"));

        let mut bytes = Vec::new();
        let mut events = SseReader::default();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
                return Err(McpError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
            if stream {
                for data in events.feed(&chunk) {
                    if let Some(result) = answer(&data, id)? {
                        return Ok((result, headers));
                    }
                }
            }
        }
        if stream {
            return Err(McpError::Malformed("the event stream ended without the answer"));
        }
        let result = answer(&bytes, id)?.ok_or(McpError::Malformed("the reply answers another request"))?;
        Ok((result, headers))
    }
}

fn transport(err: reqwest::Error) -> McpError {
    if err.is_timeout() {
        McpError::Timeout
    } else {
        McpError::Transport
    }
}

/// The result of the JSON-RPC response to request `id` in `data`; `None` for any other message,
/// as a notification in an event stream.
fn answer(data: &[u8], id: u64) -> Result<Option<Value>, McpError> {
    let message: Message =
        serde_json::from_slice(data).map_err(|_| McpError::Malformed("a reply that isn't JSON-RPC"))?;
    if message.jsonrpc != "2.0" {
        return Err(McpError::Malformed("a reply that isn't JSON-RPC 2.0"));
    }
    if message.id != Some(json!(id)) {
        return match message.id {
            None if message.method.is_some() => Ok(None),
            _ => Err(McpError::Malformed("a reply to another request")),
        };
    }
    match (message.result, message.error) {
        (Some(result), None) => Ok(Some(result)),
        (None, Some(error)) => Err(McpError::Rpc(error.code)),
        _ => Err(McpError::Malformed(
            "a reply with both or neither of a result and an error",
        )),
    }
}

#[derive(Deserialize)]
struct Message {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    protocol_version: String,
    #[serde(default)]
    capabilities: Capabilities,
}

#[derive(Default, Deserialize)]
struct Capabilities {
    #[serde(default)]
    tools: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListToolsResult {
    tools: Vec<ListedTool>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct ListedTool {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallToolResult {
    #[serde(default)]
    structured_content: Option<Value>,
    #[serde(default)]
    is_error: Option<bool>,
}

/// Splits an event stream into the data of its events, as the bytes arrive.
#[derive(Default)]
struct SseReader {
    pending: Vec<u8>,
}

impl SseReader {
    /// The data of every event this chunk completes.
    fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend(chunk.iter().filter(|&&byte| byte != b'\r'));
        let mut events = Vec::new();
        while let Some(end) = self.pending.windows(2).position(|pair| pair == b"\n\n") {
            let event: Vec<u8> = self.pending.drain(..end + 2).collect();
            let mut data = Vec::new();
            for line in event.split(|&byte| byte == b'\n') {
                if let Some(value) = line.strip_prefix(b"data:") {
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
                }
            }
            if !data.is_empty() {
                events.push(data);
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_stream_gives_the_data_of_each_event_however_it_is_cut() {
        let stream = b"event: message\r\ndata: {\"a\":1}\r\n\r\n: a comment\n\ndata: {\"b\":\ndata: 2}\n\n";
        for cut in 1..stream.len() {
            let mut reader = SseReader::default();
            let mut events = reader.feed(&stream[..cut]);
            events.extend(reader.feed(&stream[cut..]));
            assert_eq!(
                events,
                [b"{\"a\":1}".to_vec(), b"{\"b\":\n2}".to_vec()],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn only_the_answer_to_the_request_counts() {
        assert_eq!(
            answer(br#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#, 7),
            Ok(Some(json!({"ok": true})))
        );
        assert_eq!(
            answer(
                br#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#,
                7
            ),
            Ok(None),
            "a notification in the stream"
        );
        assert_eq!(
            answer(
                br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32602,"message":"x"}}"#,
                7
            ),
            Err(McpError::Rpc(-32602))
        );
        for bad in [
            &br#"{"jsonrpc":"2.0","id":8,"result":{}}"#[..],
            br#"{"jsonrpc":"1.0","id":7,"result":{}}"#,
            br#"{"jsonrpc":"2.0","id":7}"#,
            br#"{"jsonrpc":"2.0","id":7,"result":{},"error":{"code":1}}"#,
            br#"<html>"#,
        ] {
            assert!(
                matches!(answer(bad, 7), Err(McpError::Malformed(_))),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn only_a_reply_too_large_or_malformed_breaks_the_contract() {
        assert!(McpError::TooLarge.breaks_the_contract());
        assert!(McpError::Malformed("x").breaks_the_contract());
        for transient in [
            McpError::Timeout,
            McpError::Transport,
            McpError::Status(503),
            McpError::Rpc(-32602),
            McpError::ToolFailed,
        ] {
            assert!(!transient.breaks_the_contract(), "{transient}");
        }
    }

    #[test]
    fn the_token_never_prints() {
        let token = ProbeSecret::new(" probe-secret \n").unwrap();
        assert_eq!(token.expose(), "probe-secret");
        assert!(!format!("{token:?}").contains("probe-secret"));
        assert!(ProbeSecret::new("  ").is_none());
    }
}
