//! The probes the core talks to: their registration, the check of the tools they serve, calls to
//! those tools with a strict parse, quarantine, and heartbeats. A probe that breaks the contract
//! is quarantined, and the core doesn't call it again until an admin releases it.

pub mod admin;
pub mod mcp;
mod register;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::api::ProbeRegistration;
use mercury_schema::config::ProjectsConfig;
use mercury_schema::facts::{
    AskCodeReply, BlockedReply, ChangesReply, DescribeReply, HealthReply, ProgressReply, TestedReply,
};
use mercury_schema::{AccessLimit, AccessMode, ProjectId, Redactor, ToolName};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub use self::mcp::{ASK_CODE_TIMEOUT, MAX_REPLY_BYTES, MCP_TIMEOUT, McpClient, McpError, ProbeSecret};
pub use self::register::routes;
use crate::db::{self, Db};
use crate::error::ApiError;

/// A probe that hasn't registered for this long is stale.
pub const STALE_AFTER: TimeDelta = TimeDelta::minutes(3);

/// Why a registration quarantines a probe. Fixed sentences: a reason never repeats what the probe
/// sent.
const ABOVE_ACCESS_MAX: &str = "it reports an access mode above the project's access_max";
const BAD_PATTERN: &str = "its manifest has a redaction pattern that doesn't compile";

/// What the core knows about a probe, as `probes.status` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Registered within the last three minutes, and its tools checked.
    Healthy,
    /// Silent for three minutes, or not checked yet: its answers may be out of date.
    Stale,
    /// It broke the contract; never called until an admin releases it.
    Quarantined,
}

impl ProbeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Healthy => "healthy",
            ProbeStatus::Stale => "stale",
            ProbeStatus::Quarantined => "quarantined",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [ProbeStatus::Healthy, ProbeStatus::Stale, ProbeStatus::Quarantined]
            .into_iter()
            .find(|status| status.as_str() == text)
    }

    /// The status at `now`: a stored `healthy` turns `stale` once `last_seen_at` is three
    /// minutes old.
    pub fn at(self, last_seen_at: &str, now: DateTime<Utc>) -> Self {
        let silent = db::parse_time(last_seen_at).is_none_or(|seen| now - seen >= STALE_AFTER);
        match self {
            ProbeStatus::Healthy if silent => ProbeStatus::Stale,
            status => status,
        }
    }
}

pub(crate) fn access_name(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Metadata => "metadata",
        AccessMode::Code => "code",
        AccessMode::CodeModel => "code_model",
    }
}

pub(crate) fn limit_name(limit: AccessLimit) -> &'static str {
    match limit {
        AccessLimit::Token => "token",
        AccessLimit::Manifest => "manifest",
        AccessLimit::AccessMax => "access_max",
        AccessLimit::SizeCap => "size_cap",
        AccessLimit::NoCodeReader => "no_code_reader",
    }
}

/// The tool sets a `mercury.facts/0.1` probe may serve: all six, or the five without `changes`, in
/// any mode; and the seven, with the code reader's `ask_code`, only in `code_model`. A probe that
/// serves `ask_code` below `code_model` serves a tool set it has no right to.
fn allowed_tool_set(tools: &BTreeSet<ToolName>, access: AccessMode) -> bool {
    let all: BTreeSet<ToolName> = ToolName::ALL.into_iter().collect();
    let five: BTreeSet<ToolName> = all
        .iter()
        .copied()
        .filter(|tool| *tool != ToolName::Changes)
        .collect();
    let seven: BTreeSet<ToolName> = ToolName::CODE_MODEL.into_iter().collect();
    *tools == all || *tools == five || (access == AccessMode::CodeModel && *tools == seven)
}

/// A project's probe as `projects.yaml` gives it.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    pub project: String,
    /// Its MCP endpoint, such as `http://probe-delta:7000/mcp`.
    pub url: String,
    pub token: ProbeSecret,
    /// The admin's cap: a probe that reports more is quarantined.
    pub access_max: AccessMode,
}

impl ProbeConfig {
    /// Every project's probe whose token file holds a token. A project whose file is missing or
    /// empty can't register; it is reported in `skipped`, and the core runs without it.
    pub fn from_projects(projects: &ProjectsConfig) -> (Vec<ProbeConfig>, Vec<(String, std::io::ErrorKind)>) {
        let mut configs = Vec::new();
        let mut skipped = Vec::new();
        for (project, config) in &projects.projects {
            let token = std::fs::read_to_string(&config.probe.token_file)
                .map_err(|err| err.kind())
                .and_then(|text| ProbeSecret::new(&text).ok_or(std::io::ErrorKind::InvalidData));
            match token {
                Ok(token) => configs.push(ProbeConfig {
                    project: project.as_str().to_owned(),
                    url: config.probe.url.to_string(),
                    token,
                    access_max: config.access_max,
                }),
                Err(kind) => skipped.push((project.as_str().to_owned(), kind)),
            }
        }
        (configs, skipped)
    }
}

/// A tool's reply, parsed into its type: no field missing, none extra.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolReply {
    Describe(DescribeReply),
    Progress(ProgressReply),
    Tested(TestedReply),
    Blocked(BlockedReply),
    Health(HealthReply),
    Changes(ChangesReply),
    AskCode(AskCodeReply),
}

fn parse_reply(tool: ToolName, reply: Value) -> serde_json::Result<ToolReply> {
    Ok(match tool {
        ToolName::Describe => ToolReply::Describe(serde_json::from_value(reply)?),
        ToolName::Progress => ToolReply::Progress(serde_json::from_value(reply)?),
        ToolName::Tested => ToolReply::Tested(serde_json::from_value(reply)?),
        ToolName::Blocked => ToolReply::Blocked(serde_json::from_value(reply)?),
        ToolName::Health => ToolReply::Health(serde_json::from_value(reply)?),
        ToolName::Changes => ToolReply::Changes(serde_json::from_value(reply)?),
        ToolName::AskCode => ToolReply::AskCode(serde_json::from_value(reply)?),
    })
}

/// Why a probe can't answer.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("no probe is configured for that project")]
    Unknown,
    #[error("the probe hasn't registered")]
    NotRegistered,
    #[error("the probe is quarantined")]
    Quarantined,
    #[error("the probe doesn't serve that tool")]
    NotServed,
    #[error("the registration can't be stored as JSON")]
    Unstorable,
    #[error("the probe is unavailable: {0}")]
    Unavailable(McpError),
    #[error("the probe broke the contract and is quarantined: {0}")]
    BrokeContract(String),
    #[error("the database failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Audit(#[from] crate::audit::AuditError),
}

impl From<ProbeError> for ApiError {
    fn from(err: ProbeError) -> Self {
        match err {
            ProbeError::Db(_) | ProbeError::Audit(_) => {
                tracing::error!(error = %err, "cannot reach a probe's record");
                ApiError::internal()
            }
            _ => ApiError::probe_unavailable(),
        }
    }
}

/// What a registration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// Stored; its tools are checked next.
    ToCheck,
    /// Stored as a heartbeat, but the probe is quarantined.
    Quarantined(String),
}

#[derive(Debug)]
struct Probe {
    config: ProbeConfig,
    client: McpClient,
}

/// Every configured probe, and their records in the database.
#[derive(Debug, Clone)]
pub struct Probes {
    db: Db,
    probes: Arc<HashMap<String, Probe>>,
}

impl Probes {
    /// `http` is shared by every probe's MCP client; [`McpClient::http`] gives the core's.
    pub fn new(db: Db, http: reqwest::Client, configs: Vec<ProbeConfig>) -> Self {
        Self::with_timeouts(db, http, configs, MCP_TIMEOUT, ASK_CODE_TIMEOUT)
    }

    /// The same, with other times for a probe to answer: the lookups' and the code reader's. For a
    /// test that runs them in milliseconds.
    pub fn with_timeouts(
        db: Db,
        http: reqwest::Client,
        configs: Vec<ProbeConfig>,
        timeout: std::time::Duration,
        ask_code_timeout: std::time::Duration,
    ) -> Self {
        let probes = configs
            .into_iter()
            .map(|config| {
                let client = McpClient::new(http.clone(), config.url.clone(), config.token.clone())
                    .with_timeouts(timeout, ask_code_timeout);
                (config.project.clone(), Probe { config, client })
            })
            .collect();
        Self {
            db,
            probes: Arc::new(probes),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The project whose probe token this is. Every project's token is compared, each by its
    /// digest in constant time.
    pub fn project_of(&self, token: &str) -> Option<&str> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for (project, probe) in self.probes.iter() {
            let expected: [u8; 32] = Sha256::digest(probe.config.token.expose().as_bytes()).into();
            if bool::from(expected.ct_eq(&presented)) {
                found = Some(project.as_str());
            }
        }
        found
    }

    fn probe(&self, project: &str) -> Result<&Probe, ProbeError> {
        self.probes.get(project).ok_or(ProbeError::Unknown)
    }

    /// Records a registration as the project's heartbeat. A mode above the project's
    /// `access_max`, or a redaction pattern in the manifest that doesn't compile (every question
    /// about the project would fail with it), quarantines the probe at once; a quarantined probe
    /// stays quarantined. The registration that quarantines a probe writes its `probe_quarantined`
    /// audit row in the same transaction, once: a quarantined probe's heartbeats write none.
    pub async fn record(
        &self,
        registration: &ProbeRegistration,
        now: DateTime<Utc>,
    ) -> Result<Recorded, ProbeError> {
        let project = registration.project.as_str();
        let probe = self.probe(project)?;
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let previous = sqlx::query!(
            r#"SELECT status, quarantine_reason, registered_at FROM probes WHERE project = ?"#,
            project
        )
        .fetch_optional(&mut *tx)
        .await?;
        let was_quarantined = previous
            .as_ref()
            .is_some_and(|row| row.status == ProbeStatus::Quarantined.as_str());
        let (status, reason) = match &previous {
            Some(row) if was_quarantined => (ProbeStatus::Quarantined, row.quarantine_reason.clone()),
            _ if registration.access > probe.config.access_max => {
                (ProbeStatus::Quarantined, Some(ABOVE_ACCESS_MAX.to_owned()))
            }
            _ if Redactor::new(&registration.manifest.redact).is_err() => {
                (ProbeStatus::Quarantined, Some(BAD_PATTERN.to_owned()))
            }
            Some(row) => (
                ProbeStatus::parse(&row.status).unwrap_or(ProbeStatus::Stale),
                None,
            ),
            // not checked yet
            None => (ProbeStatus::Stale, None),
        };
        let registered_at = previous.map_or_else(|| db::time(now), |row| row.registered_at);
        let at = db::time(now);
        let access = access_name(registration.access);
        let limited_by = registration.access_limited_by.map(limit_name);
        let manifest = serde_json::to_string(&registration.manifest).map_err(|_| ProbeError::Unstorable)?;
        let tools = serde_json::to_string(&registration.tools).map_err(|_| ProbeError::Unstorable)?;
        let schema = "mercury.facts/0.1";
        let hash = registration.manifest_hash.as_str();
        let status_name = status.as_str();
        sqlx::query!(
            r#"INSERT INTO probes (project, url, access, access_limited_by, status, schema, manifest_hash,
                                   manifest_json, tools_json, quarantine_reason, registered_at, last_seen_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT (project) DO UPDATE SET
                   url = excluded.url, access = excluded.access, access_limited_by = excluded.access_limited_by,
                   status = excluded.status, schema = excluded.schema, manifest_hash = excluded.manifest_hash,
                   manifest_json = excluded.manifest_json, tools_json = excluded.tools_json,
                   quarantine_reason = excluded.quarantine_reason, last_seen_at = excluded.last_seen_at"#,
            project,
            probe.config.url,
            access,
            limited_by,
            status_name,
            schema,
            hash,
            manifest,
            tools,
            reason,
            registered_at,
            at
        )
        .execute(&mut *tx)
        .await?;
        if status == ProbeStatus::Quarantined && !was_quarantined {
            // no audit, no quarantine without its trail: a failed row undoes the registration, and
            // the probe's next one tries again
            let payload = serde_json::json!({ "by": "registration", "reason": reason });
            crate::audit::action_in(
                &mut tx,
                "probe_quarantined",
                Some(&registration.project),
                &payload,
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(match (status, reason) {
            (ProbeStatus::Quarantined, reason) => Recorded::Quarantined(reason.unwrap_or_default()),
            _ => Recorded::ToCheck,
        })
    }

    /// `initialize` and `tools/list` against the probe: the exact tool set of `mercury.facts/0.1`
    /// and the one it registered make it healthy; anything else quarantines it. A probe that
    /// can't be reached stays unchecked (`stale`).
    pub async fn check(&self, project: &str) -> Result<ProbeStatus, ProbeError> {
        let probe = self.probe(project)?;
        let row = sqlx::query!(
            "SELECT status, access, tools_json FROM probes WHERE project = ?",
            project
        )
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ProbeError::NotRegistered)?;
        if row.status == ProbeStatus::Quarantined.as_str() {
            return Ok(ProbeStatus::Quarantined);
        }
        // the mode it registered decides which tool set is its own; an unreadable mode is the lowest
        let access = serde_json::from_value(Value::String(row.access)).unwrap_or(AccessMode::Metadata);
        let served = match probe.client.initialize().await {
            Ok(()) => probe.client.list_tools().await,
            Err(err) => Err(err),
        };
        let served = match served {
            Ok(served) => served,
            Err(err) if err.breaks_the_contract() => {
                self.quarantine(project, &format!("initialize or tools/list: {err}"))
                    .await?;
                return Ok(ProbeStatus::Quarantined);
            }
            Err(err) => {
                tracing::warn!(project, error = %err, "a probe's tools can't be checked");
                self.set_status(project, ProbeStatus::Stale).await?;
                return Ok(ProbeStatus::Stale);
            }
        };
        let tools: Option<BTreeSet<ToolName>> = served
            .iter()
            .map(|name| serde_json::from_value(Value::String(name.clone())).ok())
            .collect();
        let registered: BTreeSet<ToolName> = serde_json::from_str::<Vec<ToolName>>(&row.tools_json)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let reason = match tools {
            None => Some("it serves a tool mercury.facts/0.1 doesn't have"),
            Some(tools) if tools.len() != served.len() || !allowed_tool_set(&tools, access) => {
                Some("it serves another tool set than mercury.facts/0.1")
            }
            Some(tools) if tools != registered => Some("it serves other tools than it registered"),
            Some(_) => None,
        };
        match reason {
            Some(reason) => {
                self.quarantine(project, reason).await?;
                Ok(ProbeStatus::Quarantined)
            }
            None => {
                self.set_status(project, ProbeStatus::Healthy).await?;
                Ok(ProbeStatus::Healthy)
            }
        }
    }

    /// Calls a tool of the project's probe and parses the reply into its type. A reply that breaks
    /// the schema or the size cap quarantines the probe; a slow or failing probe fails this call
    /// only.
    pub async fn call(
        &self,
        project: &str,
        tool: ToolName,
        arguments: Value,
    ) -> Result<ToolReply, ProbeError> {
        let probe = self.probe(project)?;
        let row = sqlx::query!("SELECT status, tools_json FROM probes WHERE project = ?", project)
            .fetch_optional(self.db.pool())
            .await?
            .ok_or(ProbeError::NotRegistered)?;
        if row.status == ProbeStatus::Quarantined.as_str() {
            return Err(ProbeError::Quarantined);
        }
        let served: Vec<ToolName> = serde_json::from_str(&row.tools_json).unwrap_or_default();
        if !served.contains(&tool) {
            return Err(ProbeError::NotServed);
        }
        // the code reader gets its own time; every other tool the lookups'
        let timeout = probe.client.timeout_for(tool);
        let reply = match probe.client.call_tool(tool.as_str(), arguments, timeout).await {
            Ok(reply) => reply,
            Err(err) if err.breaks_the_contract() => {
                let reason = format!("a {} reply: {err}", tool.as_str());
                self.quarantine(project, &reason).await?;
                return Err(ProbeError::BrokeContract(reason));
            }
            Err(err) => return Err(ProbeError::Unavailable(err)),
        };
        match parse_reply(tool, reply) {
            Ok(reply) => Ok(reply),
            Err(_) => {
                let reason = format!("a {} reply doesn't match its schema", tool.as_str());
                self.quarantine(project, &reason).await?;
                Err(ProbeError::BrokeContract(reason))
            }
        }
    }

    /// Quarantines a probe, its switch and its `probe_quarantined` row in one transaction: no
    /// quarantine without its trail, and a row that can't be written fails the call and leaves the
    /// probe as it was, so the next call that breaks the contract tries again.
    async fn quarantine(&self, project: &str, reason: &str) -> Result<(), ProbeError> {
        tracing::warn!(project, reason, "a probe is quarantined");
        let status = ProbeStatus::Quarantined.as_str();
        let payload = serde_json::json!({ "by": "check", "reason": reason });
        let id = mercury_schema::ProjectId::parse(project).ok();
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query!(
            "UPDATE probes SET status = ?, quarantine_reason = ? WHERE project = ?",
            status,
            reason,
            project
        )
        .execute(&mut *tx)
        .await?;
        crate::audit::action_in(&mut tx, "probe_quarantined", id.as_ref(), &payload, db::now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sets a status other than quarantined, unless an admin or a check quarantined it meanwhile.
    async fn set_status(&self, project: &str, status: ProbeStatus) -> Result<(), ProbeError> {
        let name = status.as_str();
        let quarantined = ProbeStatus::Quarantined.as_str();
        sqlx::query!(
            "UPDATE probes SET status = ? WHERE project = ? AND status <> ?",
            name,
            project,
            quarantined
        )
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// What the core knows of the project's probe: its mode, its status now and the manifest it
    /// registered; `None` until it has registered.
    pub async fn registered(
        &self,
        project: &str,
    ) -> Result<Option<crate::orchestrator::Registered>, ProbeError> {
        let row = sqlx::query!(
            "SELECT access, status, manifest_json, tools_json, last_seen_at FROM probes WHERE project = ?",
            project
        )
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|row| crate::orchestrator::Registered {
            access: serde_json::from_value(Value::String(row.access)).unwrap_or(AccessMode::Metadata),
            status: ProbeStatus::parse(&row.status).map_or(ProbeStatus::Stale, |status| {
                status.at(&row.last_seen_at, db::now())
            }),
            manifest: serde_json::from_str(&row.manifest_json).ok(),
            tools: serde_json::from_str(&row.tools_json).unwrap_or_default(),
        }))
    }

    /// Marks every healthy probe that hasn't registered for three minutes as stale.
    pub async fn sweep_stale(&self, now: DateTime<Utc>) -> Result<u64, ProbeError> {
        let before = db::time(now - STALE_AFTER);
        let (stale, healthy) = (ProbeStatus::Stale.as_str(), ProbeStatus::Healthy.as_str());
        let swept = sqlx::query!(
            "UPDATE probes SET status = ? WHERE status = ? AND last_seen_at <= ?",
            stale,
            healthy,
            before
        )
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(swept)
    }
}

#[async_trait::async_trait]
impl crate::orchestrator::ProbeAccess for Probes {
    async fn registered(
        &self,
        project: &ProjectId,
    ) -> Result<Option<crate::orchestrator::Registered>, ProbeError> {
        Probes::registered(self, project.as_str()).await
    }

    async fn call(&self, project: &ProjectId, tool: ToolName, args: Value) -> Result<ToolReply, ProbeError> {
        Probes::call(self, project.as_str(), tool, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        db::parse_time(text).unwrap()
    }

    #[test]
    fn a_healthy_probe_is_stale_three_minutes_after_its_last_registration() {
        let seen = "2026-10-02T10:00:00.000Z";
        assert_eq!(
            ProbeStatus::Healthy.at(seen, at("2026-10-02T10:02:59.999Z")),
            ProbeStatus::Healthy
        );
        assert_eq!(
            ProbeStatus::Healthy.at(seen, at("2026-10-02T10:03:00.000Z")),
            ProbeStatus::Stale
        );
        assert_eq!(
            ProbeStatus::Quarantined.at(seen, at("2026-10-02T11:00:00.000Z")),
            ProbeStatus::Quarantined
        );
        assert_eq!(
            ProbeStatus::Healthy.at("not a time", at(seen)),
            ProbeStatus::Stale
        );
    }

    #[test]
    fn five_or_all_six_tools_in_any_mode_and_the_seven_only_in_code_model() {
        let set = |tools: &[ToolName]| tools.iter().copied().collect::<BTreeSet<_>>();
        let modes = [AccessMode::Metadata, AccessMode::Code, AccessMode::CodeModel];
        for access in modes {
            assert!(allowed_tool_set(&set(&ToolName::ALL), access), "{access:?}");
            assert!(allowed_tool_set(&set(&ToolName::ALL[..5]), access), "{access:?}");
            assert!(
                !allowed_tool_set(&set(&ToolName::ALL[..4]), access),
                "health missing in {access:?}"
            );
            assert!(!allowed_tool_set(
                &set(&[ToolName::Describe, ToolName::Changes]),
                access
            ));
            assert!(!allowed_tool_set(&BTreeSet::new(), access));
        }
        let seven = set(&ToolName::CODE_MODEL);
        assert!(allowed_tool_set(&seven, AccessMode::CodeModel));
        for access in [AccessMode::Metadata, AccessMode::Code] {
            assert!(
                !allowed_tool_set(&seven, access),
                "ask_code below code_model, {access:?}"
            );
        }
        // the reader alone, or beside too few lookups, is no tool set of any mode
        let mut reader_and_four = set(&ToolName::ALL[..4]);
        reader_and_four.insert(ToolName::AskCode);
        assert!(!allowed_tool_set(&reader_and_four, AccessMode::CodeModel));
    }

    #[test]
    fn statuses_round_trip_through_their_names() {
        for status in [ProbeStatus::Healthy, ProbeStatus::Stale, ProbeStatus::Quarantined] {
            assert_eq!(ProbeStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ProbeStatus::parse("down"), None);
    }
}
