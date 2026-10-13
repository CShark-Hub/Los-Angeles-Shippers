//! The conformance runner: whether a probe, reached over the wire, is `mercury.facts/0.1`-compatible.
//!
//! It checks the transport and the token, the tool list, every reply against its schema type, the
//! limits, the refusals and the leak oracle over a fixture repository the probe was run on. One
//! line per check; the probe is compatible when every line is `ok`.

pub mod oracle;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mercury_schema::facts::{
    AskCodeReply, BlockedReply, ChangesReply, DescribeReply, HealthReply, ProgressReply, TestedReply,
};
use mercury_schema::{AccessMode, ToolName};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::dev;
use crate::rules::ModuleMap;

pub const USAGE: &str =
    "usage: mercury-probe-conformance --url <probe>/mcp (--token <t> | --token-file <file>) --fixture <dir>

  --url         the probe's MCP endpoint, ending in /mcp
  --token       the probe token (visible to other users of the machine; prefer --token-file)
  --token-file  a file holding the probe token
  --fixture     the repository the probe was run on, e.g. tests/fixtures/repo-delta
  --help        this text

exit: 0 the probe is v0.1-compatible, 1 a check failed, 2 the command line is wrong";

/// A reply is at most this many bytes of JSON.
const MAX_REPLY_BYTES: usize = 256 * 1024;

/// The most items a list tool returns.
const MAX_LIMIT: usize = 50;

/// A request body this large is not served.
const OVERSIZED_BODY: usize = 1024 * 1024;

/// A corpus smaller than this proves too little to be an oracle.
const MIN_CORPUS: usize = 300;

/// The newest MCP revision a v0.1 probe speaks.
const NEWEST_PROTOCOL: &str = "2025-11-25";

/// A later revision, which the runner proposes: the probe answers with [`NEWEST_PROTOCOL`].
const LATER_PROTOCOL: &str = "2026-07-28";

/// What the runner is pointed at.
#[derive(Clone)]
pub struct Config {
    /// The probe's `/mcp` endpoint.
    pub url: String,
    pub token: String,
    pub fixture: PathBuf,
}

/// The flags that take a value, each as the next argument.
const FLAGS: [&str; 4] = ["--url", "--token", "--token-file", "--fixture"];

/// The command line, after the program name. `None` for `--help`.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Config>, String> {
    let (mut url, mut token, mut token_file, mut fixture) = (None, None, None, None);
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        let slot = match flag.as_str() {
            "--url" => &mut url,
            "--token" => &mut token,
            "--token-file" => &mut token_file,
            "--fixture" => &mut fixture,
            _ => return Err(unknown(&flag)),
        };
        *slot = Some(args.next().ok_or_else(|| format!("{flag} needs a value"))?);
    }
    let url = url.ok_or("--url is required")?;
    let fixture = fixture.ok_or("--fixture is required")?;
    let token = match (token, token_file) {
        (Some(_), Some(_)) => return Err("give --token or --token-file, not both".to_owned()),
        (Some(token), None) => token,
        (None, Some(file)) => {
            std::fs::read_to_string(&file).map_err(|err| format!("cannot read the token file: {err}"))?
        }
        (None, None) => return Err("--token or --token-file is required".to_owned()),
    };
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err("the token is empty".to_owned());
    }
    if base_of(&url).is_none() {
        return Err("--url must end in /mcp".to_owned());
    }
    Ok(Some(Config {
        url,
        token,
        fixture: fixture.into(),
    }))
}

/// An argument the runner doesn't take, named without any value it carries: a mistyped
/// `--token=<t>` never prints the token, and neither does a stray word that may be part of one.
fn unknown(arg: &str) -> String {
    if !arg.starts_with('-') {
        return "an argument that isn't a flag: each value follows its flag".to_owned();
    }
    match arg.split_once('=') {
        Some((name, _)) if FLAGS.contains(&name) => {
            format!("{name} takes its value as the next argument, not after =")
        }
        Some((name, _)) => format!("unknown option {name:?} (its value not shown)"),
        None => format!("unknown option {arg:?}"),
    }
}

/// The probe's address without `/mcp`, where `/health` lives.
fn base_of(url: &str) -> Option<&str> {
    url.trim_end_matches('/').strip_suffix("/mcp")
}

/// One check: its name and what it found.
#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub result: Result<String, String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.result.is_ok())
    }

    /// The names of the checks that failed.
    pub fn failed(&self) -> Vec<&'static str> {
        self.checks
            .iter()
            .filter(|check| check.result.is_err())
            .map(|check| check.name)
            .collect()
    }

    fn push(&mut self, name: &'static str, result: Result<String, String>) {
        self.checks.push(Check { name, result });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for check in &self.checks {
            match &check.result {
                Ok(found) => writeln!(f, "ok    {:<14} {found}", check.name)?,
                Err(why) => writeln!(f, "FAIL  {:<14} {why}", check.name)?,
            }
        }
        if self.passed() {
            write!(f, "v0.1-compatible: {} checks passed", self.checks.len())
        } else {
            write!(f, "not v0.1-compatible: failed {}", self.failed().join(", "))
        }
    }
}

/// What the probe answered to one request.
struct Answer {
    status: u16,
    bytes: usize,
    body: Value,
}

impl Answer {
    /// The JSON-RPC error code, if the reply is an error.
    fn error_code(&self) -> Option<i64> {
        self.body["error"]["code"].as_i64()
    }

    /// `result.structuredContent`, if the call succeeded.
    fn structured(&self) -> Option<&Value> {
        self.body["result"].get("structuredContent")
    }
}

/// One call with the answer it got.
struct Called {
    tool: &'static str,
    arguments: Value,
    answer: Answer,
}

impl Called {
    fn label(&self) -> String {
        format!("{} {}", self.tool, self.arguments)
    }
}

/// The wire to one probe.
struct Wire {
    http: reqwest::Client,
    /// A client that keeps no connection for later: for a request the probe may refuse by closing
    /// the connection it came on (a body over its cap), so no later check picks that one up.
    alone: reqwest::Client,
    url: String,
    base: String,
    token: String,
}

impl Wire {
    fn new(config: &Config) -> Result<Self, String> {
        let base = base_of(&config.url).ok_or("the url must end in /mcp")?;
        let client = |pooled: bool| {
            let builder = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10));
            let builder = if pooled {
                builder
            } else {
                builder.pool_max_idle_per_host(0)
            };
            builder.build().map_err(|err| err.to_string())
        };
        Ok(Self {
            http: client(true)?,
            alone: client(false)?,
            url: config.url.trim_end_matches('/').to_owned(),
            base: base.to_owned(),
            token: config.token.clone(),
        })
    }

    async fn post(&self, authorization: Option<String>, body: Vec<u8>) -> Result<Answer, String> {
        Self::send(&self.http, &self.url, authorization, body).await
    }

    /// [`Wire::post`] on a connection of its own, never kept for another request.
    async fn post_alone(&self, authorization: Option<String>, body: Vec<u8>) -> Result<Answer, String> {
        Self::send(&self.alone, &self.url, authorization, body).await
    }

    async fn send(
        http: &reqwest::Client,
        url: &str,
        authorization: Option<String>,
        body: Vec<u8>,
    ) -> Result<Answer, String> {
        let mut request = http
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(body);
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        let response = request.send().await.map_err(|err| unreachable_for(&err))?;
        let status = response.status().as_u16();
        let event_stream = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let text = response.text().await.map_err(|err| unreachable_for(&err))?;
        Ok(Answer {
            status,
            bytes: text.len(),
            body: parse_body(event_stream, &text),
        })
    }

    fn envelope(method: &str, params: &Value) -> Vec<u8> {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
            .to_string()
            .into_bytes()
    }

    async fn rpc_as(
        &self,
        authorization: Option<String>,
        method: &str,
        params: Value,
    ) -> Result<Answer, String> {
        self.post(authorization, Self::envelope(method, &params)).await
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Answer, String> {
        self.rpc_as(Some(format!("Bearer {}", self.token)), method, params)
            .await
    }

    async fn call(&self, tool: &'static str, arguments: Value) -> Result<Called, String> {
        let answer = self
            .rpc("tools/call", json!({ "name": tool, "arguments": arguments }))
            .await?;
        Ok(Called {
            tool,
            arguments,
            answer,
        })
    }
}

/// How every error of the connection begins.
const UNREACHABLE: &str = "the probe ";

/// An error of the connection: what happened, never the address or a header.
fn unreachable_for(err: &reqwest::Error) -> String {
    let what = if err.is_timeout() {
        "timed out"
    } else if err.is_connect() {
        "could not connect"
    } else {
        "the connection failed"
    };
    format!("{UNREACHABLE}{what}")
}

/// The JSON of a reply, from a JSON body or the first JSON event of an event stream.
fn parse_body(event_stream: bool, text: &str) -> Value {
    if !event_stream {
        return serde_json::from_str(text).unwrap_or(Value::Null);
    }
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find_map(|data| serde_json::from_str(data.trim()).ok())
        .unwrap_or(Value::Null)
}

/// A message cut to one short line.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(140) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// Failures as one message: the first few, and how many more.
fn summary(what: &str, failures: &[String]) -> Result<String, String> {
    if failures.is_empty() {
        return Ok(what.to_owned());
    }
    let shown = failures.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
    let more = failures.len().saturating_sub(3);
    Err(if more > 0 {
        format!("{shown}; and {more} more")
    } else {
        shown
    })
}

/// Runs every check against the probe.
pub async fn run(config: &Config) -> Report {
    let mut report = Report::default();
    let wire = match Wire::new(config) {
        Ok(wire) => wire,
        Err(err) => {
            report.push("setup", Err(err));
            return report;
        }
    };
    let health = health(&wire).await;
    // a probe nobody answers for has nothing more to check
    let unreachable = health
        .as_ref()
        .err()
        .is_some_and(|why| why.starts_with(UNREACHABLE));
    report.push("health", health);
    if unreachable {
        return report;
    }
    report.push("auth", auth(&wire).await);
    report.push("initialize", initialize(&wire).await);
    // the modules every check goes over: the fixture's, whatever `describe` lists
    let modules = fixture_modules(&config.fixture);
    report.push("tools", tools(&wire, &modules).await);

    let gathered = match &modules {
        Ok(modules) => gather(&wire, modules).await,
        Err(err) => Err(err.clone()),
    };
    let corpus = oracle::corpus_of(&config.fixture);
    match &gathered {
        Ok(gathered) => {
            report.push("schemas", schemas(gathered));
            report.push("invalid_params", invalid_params(&wire, gathered).await);
            report.push("limits", limits(&wire, gathered).await);
        }
        Err(err) => {
            for name in ["schemas", "invalid_params", "limits"] {
                report.push(name, Err(format!("nothing to check: {err}")));
            }
        }
    }
    let oracle_result = match (&gathered, corpus) {
        (Ok(gathered), Ok(corpus)) => leak_oracle(&wire, gathered, &corpus).await,
        (Err(err), _) => Err(format!("nothing to check: {err}")),
        (_, Err(err)) => Err(err),
    };
    report.push("leak_oracle", oracle_result);
    report
}

/// `GET /health` answers without a token.
async fn health(wire: &Wire) -> Result<String, String> {
    let response = wire
        .http
        .get(format!("{}/health", wire.base))
        .send()
        .await
        .map_err(|err| unreachable_for(&err))?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status != 200 || body["status"] != "ok" || !body["version"].is_string() {
        return Err(format!(
            "GET /health without a token must answer 200 with status and version, got {status}"
        ));
    }
    Ok("answers without a token".to_owned())
}

/// A request without the token, or with another, never reaches MCP.
async fn auth(wire: &Wire) -> Result<String, String> {
    let params = json!({ "name": "describe", "arguments": {} });
    let refused = [
        ("no token", None),
        ("a wrong token", Some("Bearer wrong-token".to_owned())),
        ("an empty token", Some("Bearer ".to_owned())),
        ("another scheme", Some(format!("Basic {}", wire.token))),
    ];
    for (what, authorization) in refused {
        let answer = wire.rpc_as(authorization, "tools/call", params.clone()).await?;
        if answer.status != 401 {
            return Err(format!("{what} must get 401, got {}", answer.status));
        }
    }
    let answer = wire.rpc("tools/call", params).await?;
    if answer.status != 200 {
        return Err(format!("the token was refused: {}", answer.status));
    }
    Ok("401 without the token, with another and with another scheme".to_owned())
}

/// `initialize` answers with the tools capability, and a client that proposes a later revision
/// than the probe speaks is answered with the newest it does.
async fn initialize(wire: &Wire) -> Result<String, String> {
    let answer = wire
        .rpc(
            "initialize",
            json!({ "protocolVersion": LATER_PROTOCOL, "capabilities": {}, "clientInfo": { "name": "conformance", "version": "0" } }),
        )
        .await?;
    let result = &answer.body["result"];
    if answer.status != 200 || !result["capabilities"]["tools"].is_object() {
        return Err("initialize must answer 200 with the tools capability".to_owned());
    }
    if result["protocolVersion"] != NEWEST_PROTOCOL {
        return Err(format!(
            "a client that proposes {LATER_PROTOCOL} must be answered with {NEWEST_PROTOCOL}, got {}",
            short(&result["protocolVersion"].to_string())
        ));
    }
    Ok(format!(
        "the tools capability, at {NEWEST_PROTOCOL} to a client that proposes {LATER_PROTOCOL}"
    ))
}

/// The tool list equals the spec's for the access mode `describe` names, read first: the six
/// tools, or seven under `code_model`; each tool once, with both schemas. `describe` is checked,
/// not trusted: its `tools` names the same ones and its `modules` the fixture manifest's.
async fn tools(wire: &Wire, modules: &Result<Vec<String>, String>) -> Result<String, String> {
    let describe = wire.call("describe", json!({})).await?;
    let structured = describe
        .answer
        .structured()
        .ok_or("describe answered no structured content")?;
    let expected_set = expected_tools(&structured["access"])?;
    let (number, set) = in_words(expected_set);
    let expected_names: Vec<&str> = expected_set.iter().map(|t| t.as_str()).collect();

    let answer = wire.rpc("tools/list", json!({})).await?;
    let listed = answer.body["result"]["tools"]
        .as_array()
        .ok_or("tools/list answered no tools")?;
    let names: Vec<&str> = listed.iter().filter_map(|t| t["name"].as_str()).collect();
    let expected: BTreeSet<&str> = expected_names.iter().copied().collect();
    let got: BTreeSet<&str> = names.iter().copied().collect();
    if got != expected || names.len() != expected.len() {
        return Err(format!(
            "the tool list must be exactly the {set} {expected_names:?}, each once, got {names:?}"
        ));
    }
    for tool in listed {
        for schema in ["inputSchema", "outputSchema"] {
            if !tool[schema].is_object() {
                return Err(format!(
                    "{} advertises no {schema}",
                    tool["name"].as_str().unwrap_or_default()
                ));
            }
        }
    }

    let mut failures = Vec::new();
    if let Some(diff) = differs(&names_in(&structured["tools"]), &expected_names) {
        failures.push(format!(
            "describe's tools aren't the {number} of the tool list: {diff}"
        ));
    }
    match modules {
        Ok(modules) => {
            if let Some(diff) = differs(&names_in(&structured["modules"]), modules) {
                failures.push(format!(
                    "describe's modules aren't the fixture manifest's: {diff}"
                ));
            }
        }
        Err(err) => failures.push(format!("nothing to compare describe's modules with: {err}")),
    }
    summary(
        &format!(
            "{set}, each with an input and an output schema; describe names them and the fixture's {} modules",
            modules.as_ref().map_or(0, Vec::len)
        ),
        &failures,
    )
}

/// The tools a probe in the access mode `describe` names must list: the six, or seven with the
/// code reader's under `code_model`. A mode the spec doesn't name is an error.
fn expected_tools(access: &Value) -> Result<&'static [ToolName], String> {
    let mode: AccessMode = serde_json::from_value(access.clone()).map_err(|_| {
        format!(
            "describe's access isn't a mode of the spec: {}",
            short(&access.to_string())
        )
    })?;
    Ok(ToolName::for_access(mode))
}

/// A tool set in words: as a number (`six`, `seven (code_model)`) and with its noun (`six tools`,
/// `seven tools (code_model)`).
fn in_words(tools: &[ToolName]) -> (&'static str, &'static str) {
    if tools.contains(&ToolName::AskCode) {
        ("seven (code_model)", "seven tools (code_model)")
    } else {
        ("six", "six tools")
    }
}

/// The modules a probe on the fixture names: its manifest's, read as the probe reads them, then
/// `Other`, for work outside every module.
fn fixture_modules(fixture: &Path) -> Result<Vec<String>, String> {
    let (manifest, _) = dev::read_manifest(fixture).map_err(|err| format!("the fixture: {err}"))?;
    let map = ModuleMap::new(&manifest).map_err(|err| format!("the fixture: {err}"))?;
    Ok(map
        .modules()
        .iter()
        .map(|module| module.as_str().to_owned())
        .collect())
}

/// The names a reply lists, each as it is; an entry that isn't a string stands as its JSON.
fn names_in(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().map_or_else(|| item.to_string(), str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// How a list of names differs from the one it must be: the names missing, the names extra and the
/// names listed more than once. `None` when it is the same, each name once.
fn differs(listed: &[String], expected: &[impl AsRef<str>]) -> Option<String> {
    let expected: BTreeSet<&str> = expected.iter().map(AsRef::as_ref).collect();
    let got: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let missing: Vec<&str> = expected.difference(&got).copied().collect();
    let extra: Vec<&str> = got.difference(&expected).copied().collect();
    let mut seen = BTreeSet::new();
    let twice: Vec<&str> = listed
        .iter()
        .map(String::as_str)
        .filter(|name| !seen.insert(*name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing {missing:?}"));
    }
    if !extra.is_empty() {
        parts.push(format!("extra {extra:?}"));
    }
    if !twice.is_empty() {
        parts.push(format!("more than once {twice:?}"));
    }
    (!parts.is_empty()).then(|| short(&parts.join(", ")))
}

/// Every call the oracle makes, with its answer.
struct Gathered {
    modules: Vec<String>,
    access: String,
    calls: Vec<Called>,
    refused: Vec<Called>,
}

/// Every call of the oracle over the fixture's modules, whatever `describe` lists.
async fn gather(wire: &Wire, modules: &[String]) -> Result<Gathered, String> {
    let mut calls = Vec::new();
    for (tool, arguments) in oracle::calls(modules) {
        calls.push(wire.call(tool, arguments).await?);
    }
    let mut refused = Vec::new();
    for (tool, arguments) in oracle::refused() {
        refused.push(wire.call(tool, arguments).await?);
    }
    // the mode the probe serves is `describe`'s to say
    let access = calls
        .iter()
        .find(|called| called.tool == "describe")
        .and_then(|called| called.answer.structured())
        .and_then(|structured| structured["access"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    // `ask_code` belongs to `code_model`: asked there, and refused as an unknown tool anywhere else
    let code_model = access == "code_model";
    if code_model {
        for (tool, arguments) in oracle::ask_code_calls() {
            calls.push(wire.call(tool, arguments).await?);
        }
    }
    for (tool, arguments) in oracle::refused_ask_code(code_model) {
        refused.push(wire.call(tool, arguments).await?);
    }
    Ok(Gathered {
        modules: modules.to_vec(),
        access,
        calls,
        refused,
    })
}

/// Every reply parses into its schema type: nothing missing, nothing extra, no free text.
fn schemas(gathered: &Gathered) -> Result<String, String> {
    let mut failures = Vec::new();
    for called in &gathered.calls {
        if let Err(why) = check_reply(called) {
            failures.push(format!("{}: {why}", called.label()));
        }
    }
    summary(
        &format!(
            "{} replies, all of their schema type ({} mode)",
            gathered.calls.len(),
            gathered.access
        ),
        &failures,
    )
}

fn check_reply(called: &Called) -> Result<(), String> {
    let answer = &called.answer;
    if answer.status != 200 || answer.body.get("error").is_some() {
        return Err(format!("not answered: status {}", answer.status));
    }
    if answer.body["result"]["isError"] == true {
        return Err("a tool error".to_owned());
    }
    let structured = answer.structured().ok_or("no structured content")?;
    if structured["schema"] != "mercury.facts/0.1" {
        return Err("the reply's schema id isn't mercury.facts/0.1".to_owned());
    }
    // the text content carries the same JSON
    let text: Option<Value> = answer.body["result"]["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|first| first["text"].as_str())
        .and_then(|text| serde_json::from_str(text).ok());
    if text.as_ref() != Some(structured) {
        return Err("the text content isn't the structured content".to_owned());
    }
    match called.tool {
        "describe" => parse::<DescribeReply>(structured),
        "progress" => parse::<ProgressReply>(structured),
        "tested" => parse::<TestedReply>(structured),
        "blocked" => parse::<BlockedReply>(structured),
        "health" => parse::<HealthReply>(structured),
        "changes" => parse::<ChangesReply>(structured),
        "ask_code" => parse::<AskCodeReply>(structured),
        other => Err(format!("{other} has no schema type")),
    }
}

fn parse<T: DeserializeOwned>(value: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|err| short(&err.to_string()))
}

/// Arguments that don't parse and unknown tools and modules are `invalid_params`, and the error
/// is a fixed sentence that never echoes an argument.
async fn invalid_params(wire: &Wire, gathered: &Gathered) -> Result<String, String> {
    let mut failures = Vec::new();
    let mut cases: Vec<&Called> = gathered.refused.iter().collect();
    let marked = [
        ("progress", json!({ "module": "ZqEcho-Marker-7" })),
        ("progress", json!({ "since": "ZqEchoMarker7" })),
        (
            "changes",
            json!({ "since": "2026-09-01", "module": "ZqEcho-Marker-7" }),
        ),
    ];
    let mut extra = Vec::new();
    for (tool, arguments) in marked {
        extra.push(wire.call(tool, arguments).await?);
    }
    cases.extend(extra.iter());
    for called in cases {
        if called.answer.error_code() != Some(-32602) {
            failures.push(format!("{} wasn't refused with invalid_params", called.label()));
            continue;
        }
        let message = called.answer.body["error"]["message"]
            .as_str()
            .unwrap_or_default();
        let mut values = Vec::new();
        oracle::strings(&called.arguments, &mut values);
        if let Some(echoed) = values
            .iter()
            .find(|v| v.len() >= 4 && message.contains(v.as_str()))
        {
            failures.push(format!("{} echoed its argument {echoed:?}", called.label()));
        }
    }
    summary(
        &format!(
            "{} refusals, none echoing an argument",
            gathered.refused.len() + 3
        ),
        &failures,
    )
}

/// The `limit` bounds, the reply cap and the request body cap.
async fn limits(wire: &Wire, gathered: &Gathered) -> Result<String, String> {
    let mut failures = Vec::new();
    let items = |called: &Called| {
        called
            .answer
            .structured()
            .and_then(|s| s["items"].as_array())
            .map(Vec::len)
    };
    let find = |limit: Option<u64>| {
        gathered.calls.iter().find(|called| {
            called.tool == "progress"
                && called.arguments.get("module").is_none()
                && called.arguments.get("since").is_none()
                && called.arguments.get("limit").and_then(Value::as_u64) == limit
        })
    };
    let default = find(None).and_then(items);
    match default {
        Some(len) if len <= MAX_LIMIT => {}
        other => failures.push(format!(
            "progress without a limit must give at most {MAX_LIMIT} items, got {other:?}"
        )),
    }
    for limit in [1u64, 50] {
        let len = find(Some(limit)).and_then(items);
        let allowed = usize::try_from(limit).unwrap_or(MAX_LIMIT);
        match len {
            Some(len) if len <= allowed && len == allowed.min(default.unwrap_or(0)) => {}
            other => failures.push(format!(
                "progress with limit {limit} must give {} items, got {other:?}",
                allowed.min(default.unwrap_or(0))
            )),
        }
    }
    let largest = gathered
        .calls
        .iter()
        .chain(&gathered.refused)
        .map(|called| called.answer.bytes)
        .max()
        .unwrap_or(0);
    if largest > MAX_REPLY_BYTES {
        failures.push(format!(
            "a reply of {largest} bytes is over the {MAX_REPLY_BYTES}-byte cap"
        ));
    }
    // a valid call that only a body cap can refuse: a `describe` reply to it means there is none;
    // sent alone, since a probe may close the connection it refuses a body on
    let refused = match wire
        .post_alone(Some(format!("Bearer {}", wire.token)), oversized_describe())
        .await
    {
        Ok(answer) if served(&answer) => {
            failures.push(format!(
                "a 1 MiB request body (a valid describe padded with whitespace) was served with {}: the probe has no body cap",
                answer.status
            ));
            String::new()
        }
        Ok(answer) => format!("refused with {}", answer.status),
        Err(_) => "refused by a closed connection".to_owned(),
    };
    summary(
        &format!(
            "limit 1 and 50, the default of {MAX_LIMIT}, replies under 256 KB (largest {largest} bytes), a 1 MiB describe {refused}"
        ),
        &failures,
    )
}

/// The oversized request: a valid `describe` call, padded to [`OVERSIZED_BODY`] bytes with JSON
/// whitespace inside its arguments. A probe that reads it whole can only serve it, so nothing but a
/// body cap refuses it.
fn oversized_describe() -> Vec<u8> {
    let head = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"describe","arguments":{"#;
    let tail = b"}}}";
    let mut body = Vec::with_capacity(OVERSIZED_BODY);
    body.extend_from_slice(head);
    body.resize(OVERSIZED_BODY - tail.len(), b' ');
    body.extend_from_slice(tail);
    body
}

/// Whether the probe served a call: a success status with a JSON-RPC result. Anything else, a
/// refusing status or an error, is not serving it.
fn served(answer: &Answer) -> bool {
    (200..300).contains(&answer.status) && answer.body.get("result").is_some()
}

/// None of the fixture's paths, code lines or distinctive identifiers is in any reply.
async fn leak_oracle(wire: &Wire, gathered: &Gathered, corpus: &BTreeSet<String>) -> Result<String, String> {
    if corpus.len() < MIN_CORPUS {
        return Err(format!(
            "the fixture's corpus has {} strings; fewer than {MIN_CORPUS} proves too little",
            corpus.len()
        ));
    }
    let mut failures = Vec::new();
    let listed = wire.rpc("tools/list", json!({})).await?;
    for (what, body) in [("tools/list".to_owned(), &listed.body)].into_iter().chain(
        gathered
            .calls
            .iter()
            .chain(&gathered.refused)
            .map(|called| (called.label(), &called.answer.body)),
    ) {
        let found = oracle::leaks(corpus, body);
        if let Some(first) = found.first() {
            failures.push(format!(
                "{what} leaked {} fixture strings, among them {:?}",
                found.len(),
                short(first)
            ));
        }
    }
    let total = gathered.calls.len() + gathered.refused.len() + 1;
    summary(
        &format!(
            "{} corpus strings, {total} replies over {} modules in {} mode, no hit",
            corpus.len(),
            gathered.modules.len(),
            gathered.access
        ),
        &failures,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Option<Config>, String> {
        parse_args(list.iter().map(|arg| (*arg).to_owned()))
    }

    /// The body is exactly 1 MiB and the same call as a `describe` without padding: nothing in it
    /// but its size can be refused.
    #[test]
    fn the_oversized_request_is_a_valid_describe_padded_with_whitespace() {
        let body = oversized_describe();
        assert_eq!(body.len(), OVERSIZED_BODY);
        let call: Value = serde_json::from_slice(&body).unwrap();
        let plain: Value = serde_json::from_slice(&Wire::envelope(
            "tools/call",
            &json!({ "name": "describe", "arguments": {} }),
        ))
        .unwrap();
        assert_eq!(call, plain);
        // padded inside the arguments, so a reader has to take all of it to reach the call's end
        assert!(body.ends_with(b" }}}"));
    }

    /// The fixture's modules are its manifest's, in its order, then `Other`, as the probe on it
    /// lists them; a folder with no manifest has none to give.
    #[test]
    fn the_fixture_modules_are_its_manifests_then_other() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
        assert_eq!(
            fixture_modules(&fixture).unwrap(),
            ["Invoicing", "Export", "Auth", "Other"]
        );
        let err = fixture_modules(&fixture.join("meta")).err().unwrap();
        assert!(err.starts_with("the fixture: cannot read the manifest"), "{err}");
    }

    /// A list of names is the expected one only with each name once; what differs is named.
    #[test]
    fn a_list_of_names_names_what_differs() {
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let expected = ["describe", "progress", "health"];
        assert_eq!(
            differs(&names(&["health", "describe", "progress"]), &expected),
            None
        );
        assert_eq!(
            differs(&names(&["describe", "progress", "health", "search"]), &expected).unwrap(),
            r#"extra ["search"]"#
        );
        assert_eq!(
            differs(&names(&["describe", "progress", "diff"]), &expected).unwrap(),
            r#"missing ["health"], extra ["diff"]"#
        );
        assert_eq!(
            differs(&names(&["describe", "progress", "health", "health"]), &expected).unwrap(),
            r#"more than once ["health"]"#
        );
        assert_eq!(
            differs(&names_in(&json!(["describe", 7, "health"])), &expected).unwrap(),
            r#"missing ["progress"], extra ["7"]"#
        );
        assert_eq!(
            differs(&names_in(&Value::Null), &expected).unwrap(),
            r#"missing ["describe", "health", "progress"]"#
        );
    }

    /// `metadata` and `code` list the six tools, `code_model` adds the code reader's; a mode the
    /// spec doesn't name has no tool set, and the message names it.
    #[test]
    fn the_expected_tools_follow_the_access_mode() {
        assert_eq!(expected_tools(&json!("metadata")).unwrap(), ToolName::ALL);
        assert_eq!(expected_tools(&json!("code")).unwrap(), ToolName::ALL);
        assert_eq!(
            expected_tools(&json!("code_model")).unwrap(),
            ToolName::CODE_MODEL
        );
        assert_eq!(in_words(&ToolName::ALL), ("six", "six tools"));
        assert_eq!(
            in_words(&ToolName::CODE_MODEL),
            ("seven (code_model)", "seven tools (code_model)")
        );
        for other in [
            json!("code-model"),
            json!("Code"),
            json!(""),
            json!(null),
            json!(2),
            json!({ "access": "code" }),
        ] {
            let err = expected_tools(&other).err().unwrap();
            assert_eq!(
                err,
                format!("describe's access isn't a mode of the spec: {other}")
            );
        }
    }

    /// A misspelt option never carries its value into the message: not after `=`, and not a stray
    /// word that may be part of a token.
    #[test]
    fn an_unknown_option_is_named_without_its_value() {
        let secret = "mcy_probe_s3cret_value";
        for (given, named) in [
            (format!("--token={secret}"), "--token"),
            (format!("--tokn={secret}"), "--tokn"),
            (format!("--url=http://127.0.0.1:1/mcp?{secret}"), "--url"),
            ("--verbose".to_owned(), "--verbose"),
        ] {
            let err = args(&[&given, "--fixture", "f"]).err().unwrap();
            assert!(err.contains(named), "{given}: {err}");
            assert!(!err.contains(secret), "{given}: {err}");
        }
        let err = args(&["--url", "http://127.0.0.1:1/mcp", secret]).err().unwrap();
        assert!(err.contains("isn't a flag"), "{err}");
        assert!(!err.contains(secret), "{err}");
    }
}
