//! MCP over Streamable HTTP at `/mcp`, behind the probe token, and `GET /health` beside it.

use std::borrow::Cow;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use mercury_schema::tools::tool_schemas;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use sha2::{Digest, Sha256};

use crate::tools::{ToolError, Tools};

/// The newest MCP revision the probe speaks; a client that proposes a later one is answered with it.
const NEWEST_PROTOCOL: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// The probe token callers must present. `None` refuses every `/mcp` request.
#[derive(Clone)]
pub struct ProbeToken(Option<Arc<[u8; 32]>>);

impl ProbeToken {
    pub fn new(token: Option<&str>) -> Self {
        Self(
            token
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| Arc::new(Sha256::digest(t.as_bytes()).into())),
        )
    }

    /// Compares digests in constant time, so the answer's timing says nothing about the token.
    fn accepts(&self, presented: &str) -> bool {
        let Some(expected) = &self.0 else {
            return false;
        };
        let got: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        got.iter()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl std::fmt::Debug for ProbeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ProbeToken(<set>)"
        } else {
            "ProbeToken(<none>)"
        })
    }
}

async fn require_token(State(token): State<ProbeToken>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(presented) if token.accepts(presented) => next.run(request).await,
        _ => (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")]).into_response(),
    }
}

/// The MCP server: one per request, all sharing the same tools.
#[derive(Clone)]
struct Server {
    tools: Tools,
}

impl ServerHandler for Server {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mercury-probe", env!("CARGO_PKG_VERSION")))
    }

    /// The revisions whose results these tools build, as the core's: a client that proposes a later
    /// one is answered with the newest of them, and a request that names a later one is refused
    /// with them. The library knows a later revision, whose `tools/list` must also say how long it
    /// may be cached; serving it would hand that client a list it rejects.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&NEWEST_PROTOCOL))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(list(self.tools.served())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        list(self.tools.served())
            .into_iter()
            .find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request
            .arguments
            .map_or(serde_json::Value::Null, serde_json::Value::Object);
        match self.tools.call(&request.name, arguments).await {
            Ok(reply) => Ok(CallToolResult::structured(reply).into()),
            Err(
                err @ (ToolError::UnknownTool
                | ToolError::InvalidArguments
                | ToolError::UnknownModule
                | ToolError::TooOld),
            ) => Err(ErrorData::invalid_params(err.to_string(), None)),
            Err(
                err @ (ToolError::NotReady
                | ToolError::TooLarge
                | ToolError::SnapshotTooLarge
                | ToolError::ReaderFailed),
            ) => Err(ErrorData::internal_error(err.to_string(), None)),
        }
    }
}

/// The tools served, with their input and output schemas from `mercury-schema`.
fn list(served: &[mercury_schema::ToolName]) -> Vec<Tool> {
    served
        .iter()
        .copied()
        .map(|tool| {
            let (input, output) = tool_schemas(tool);
            let mut entry = Tool::new(tool.as_str(), description(tool), object(input));
            entry.output_schema = Some(Arc::new(object(output)));
            entry
        })
        .collect()
}

fn object(schema: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match schema {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn description(tool: mercury_schema::ToolName) -> &'static str {
    use mercury_schema::ToolName as T;
    match tool {
        T::Describe => "The project, its modules, the access mode and the snapshot the facts come from.",
        T::Progress => "Pieces of work and their status, the most recently updated first.",
        T::Tested => "Tests and CI results per module.",
        T::Blocked => "Blocked work and what it waits on.",
        T::Health => "Build state, CI pass rate, open pull requests, TODO markers and a health score.",
        T::Changes => "Commits and merged pull requests per module since a date.",
        T::AskCode => {
            "Findings about the code for one question, from the project's code reader; served only in code_model."
        }
    }
}

/// `GET /health` and, behind the token, `/mcp`.
pub fn app(tools: Tools, token: ProbeToken) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        // arguments are small; a large body is refused before it is parsed
        .with_max_request_body_bytes(64 * 1024)
        // the probe sits on the probes network, reached by the core's name; the token guards it
        .disable_allowed_hosts();
    let service = StreamableHttpService::new(
        move || Ok(Server { tools: tools.clone() }),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let mcp = Router::new()
        .fallback_service(service)
        .layer(middleware::from_fn_with_state(token, require_token));
    crate::router().nest("/mcp", mcp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_compared_by_digest() {
        let token = ProbeToken::new(Some("mcy_probe_secret\n"));
        assert!(token.accepts("mcy_probe_secret"));
        assert!(!token.accepts("mcy_probe_secreT"));
        assert!(!token.accepts(""));
        assert!(!ProbeToken::new(None).accepts(""));
        assert!(!ProbeToken::new(Some("  ")).accepts(""));
        assert!(!format!("{token:?}").contains("secret"));
    }

    use mercury_schema::AccessMode;

    fn names_of(tools: &[Tool]) -> Vec<&str> {
        tools.iter().map(|t| t.name.as_ref()).collect()
    }

    #[test]
    fn six_tools_with_both_schemas() {
        for access in [AccessMode::Metadata, AccessMode::Code] {
            let tools = list(crate::tools::served(access));
            assert_eq!(
                names_of(&tools),
                ["describe", "progress", "tested", "blocked", "health", "changes"]
            );
            assert!(
                tools
                    .iter()
                    .all(|t| t.output_schema.is_some() && !t.input_schema.is_empty())
            );
        }
    }

    #[test]
    fn seven_tools_under_code_model_with_both_schemas() {
        let tools = list(crate::tools::served(AccessMode::CodeModel));
        assert_eq!(
            names_of(&tools),
            [
                "describe", "progress", "tested", "blocked", "health", "changes", "ask_code"
            ]
        );
        assert!(
            tools
                .iter()
                .all(|t| t.output_schema.is_some() && !t.input_schema.is_empty())
        );
        // the seventh takes a question and nothing else
        let ask = tools.last().unwrap();
        assert_eq!(ask.input_schema["properties"].as_object().unwrap().len(), 1);
        assert!(ask.input_schema["properties"].get("question").is_some());
    }
}
