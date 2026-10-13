//! The tools, answered from the index: arguments parsed into `mercury-schema` types, replies
//! built from them, so a reply can only hold enums, numbers, dates, ids and `SafeText`. Six of
//! them, and in `code_model` a seventh, `ask_code`, whose findings come from the code reader.

use chrono::NaiveTime;
use mercury_schema::api::Usd;
use mercury_schema::facts::{
    AskCodeReply, BlockedReply, ChangeSummary, ChangesReply, DescribeReply, ProgressReply, SchemaId, Status,
    TestedReply,
};
use mercury_schema::redact::{redacted_question, safe_text};
use mercury_schema::tools::{
    AskCodeArgs, BlockedArgs, ChangesArgs, DescribeArgs, HealthArgs, Limit, ProgressArgs, TestedArgs,
};
use mercury_schema::{AccessLimit, AccessMode, Fact, ModuleName, ProjectId, Redactor, SafeText, ToolName};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use crate::ask_code::{self, Corpus, RequestError, Snapshot};
use crate::index::Index;
use crate::probe::Probe;
use crate::reader::client::{ReadFailure, ReaderClient};
use crate::rules::ModuleMap;

/// The tools a probe serves in `access`: the six, and `ask_code` only in `code_model`.
pub fn served(access: AccessMode) -> &'static [ToolName] {
    ToolName::for_access(access)
}

/// How many titles `changes` names per module.
pub const NOTABLE: u32 = 5;

/// The largest reply, in bytes of JSON.
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// Why a call failed. The messages are fixed: none of them echoes an argument.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool")]
    UnknownTool,
    #[error("the arguments don't match the tool's input schema")]
    InvalidArguments,
    #[error("unknown module")]
    UnknownModule,
    #[error("the date is before the period the probe keeps")]
    TooOld,
    #[error("the index isn't ready")]
    NotReady,
    #[error("the reply is larger than the limit")]
    TooLarge,
    #[error("the snapshot is larger than the code reader reads")]
    SnapshotTooLarge,
    #[error("the code reader gave no answer")]
    ReaderFailed,
}

/// What `describe` says about the project: set from the probe the index was last built from.
#[derive(Debug, Clone)]
struct About {
    project: ProjectId,
    display_name: SafeText,
    access: AccessMode,
    access_limited_by: Option<AccessLimit>,
    modules: Vec<ModuleName>,
    /// What `ask_code` reads: the snapshot's folder, the files the listing kept, and the
    /// manifest's redaction rules.
    repo: PathBuf,
    files: Option<Vec<String>>,
    redactor: Redactor,
}

impl About {
    fn new(probe: &Probe) -> Result<Self, String> {
        let modules = ModuleMap::new(&probe.manifest)?.modules();
        // the manifest's display name leaves as SafeText too; a name that fails falls back to the id
        let display_name = safe_text(&probe.manifest.display_name, &probe.redactor)
            .or_else(|_| safe_text(probe.manifest.project.as_str(), &probe.redactor))
            .map_err(|_| "the manifest's display name can't be shown, nor its project id".to_owned())?;
        Ok(Self {
            project: probe.manifest.project.clone(),
            display_name,
            access: probe.access,
            access_limited_by: probe.access_limited_by,
            modules,
            repo: probe.repo.clone(),
            files: probe.files.clone(),
            redactor: probe.redactor.clone(),
        })
    }
}

/// The code reader as `ask_code` calls it, and what was last said about its failing.
#[derive(Debug)]
struct CodeReader {
    client: ReaderClient,
    /// The line logged for the last failure, so the same one is logged once until it changes.
    noted: Mutex<Option<String>>,
}

impl CodeReader {
    fn note(&self, line: Option<String>) {
        let mut noted = self.noted.lock().unwrap_or_else(PoisonError::into_inner);
        if *noted != line {
            if let Some(line) = &line {
                eprintln!("mercury-probe: {line}");
            }
            *noted = line;
        }
    }
}

/// The tools over an index. Clones share the index and what `describe` says, so a rebuild that
/// moves the mode or the manifest reaches every server at once ([`Self::set`]).
#[derive(Debug, Clone)]
pub struct Tools {
    index: Index,
    about: Arc<RwLock<Option<About>>>,
    max_reply_bytes: usize,
    /// The code reader, when one is configured: without it `ask_code` is never served.
    reader: Option<Arc<CodeReader>>,
}

impl Tools {
    pub fn new(index: Index, probe: &Probe) -> Result<Self, String> {
        let tools = Self::waiting(index);
        tools.set(probe)?;
        Ok(tools)
    }

    /// Tools that answer `NotReady` until [`Self::set`] is called: `serve` before `sync` has
    /// written its first metadata.
    pub fn waiting(index: Index) -> Self {
        Self {
            index,
            about: Arc::new(RwLock::new(None)),
            max_reply_bytes: MAX_REPLY_BYTES,
            reader: None,
        }
    }

    /// The same, with a code reader to call: in `code_model`, `ask_code` is served.
    #[must_use]
    pub fn with_reader(mut self, client: ReaderClient) -> Self {
        self.reader = Some(Arc::new(CodeReader {
            client,
            noted: Mutex::new(None),
        }));
        self
    }

    /// The tools served now: the six, and `ask_code` only while the probe's mode is `code_model`
    /// and it has a reader to call. Before the first index, the six.
    pub fn served(&self) -> &'static [ToolName] {
        let access = self
            .about
            .read()
            .ok()
            .and_then(|about| about.as_ref().map(|about| about.access));
        match access {
            Some(AccessMode::CodeModel) if self.reader.is_some() => served(AccessMode::CodeModel),
            _ => served(AccessMode::Code),
        }
    }

    /// What `describe` says from now on: the probe the index was just rebuilt from.
    pub fn set(&self, probe: &Probe) -> Result<(), String> {
        let about = About::new(probe)?;
        let mut slot = self
            .about
            .write()
            .map_err(|_| "the tools' state is poisoned".to_owned())?;
        *slot = Some(about);
        Ok(())
    }

    fn about(&self) -> Result<About, ToolError> {
        self.about
            .read()
            .map_err(|_| ToolError::NotReady)?
            .clone()
            .ok_or(ToolError::NotReady)
    }

    /// A lower reply cap, for the tests.
    pub fn with_max_reply_bytes(mut self, bytes: usize) -> Self {
        self.max_reply_bytes = bytes;
        self
    }

    /// Calls a tool by its MCP name with raw JSON arguments, and returns its reply as JSON.
    pub async fn call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let tool = self
            .served()
            .iter()
            .copied()
            .find(|tool| tool.as_str() == name)
            .ok_or(ToolError::UnknownTool)?;
        let reply = match tool {
            ToolName::Describe => {
                let DescribeArgs {} = parse(arguments)?;
                self.reply(&self.describe().await?)
            }
            ToolName::Progress => self.reply(&self.progress(parse(arguments)?).await?),
            ToolName::Tested => self.reply(&self.tested(parse(arguments)?).await?),
            ToolName::Blocked => self.reply(&self.blocked(parse(arguments)?).await?),
            ToolName::Health => {
                let HealthArgs {} = parse(arguments)?;
                self.reply(&self.index.health().await.map_err(|_| ToolError::NotReady)?)
            }
            ToolName::Changes => self.reply(&self.changes(parse(arguments)?).await?),
            ToolName::AskCode => self.reply(&self.ask_code(parse(arguments)?).await?),
        }?;
        Ok(reply)
    }

    fn reply<T: Serialize>(&self, reply: &T) -> Result<serde_json::Value, ToolError> {
        let bytes = serde_json::to_vec(reply).map_err(|_| ToolError::NotReady)?;
        if bytes.len() > self.max_reply_bytes {
            return Err(ToolError::TooLarge);
        }
        serde_json::from_slice(&bytes).map_err(|_| ToolError::NotReady)
    }

    async fn describe(&self) -> Result<DescribeReply, ToolError> {
        let about = self.about()?;
        let indexed = self.index.snapshot().await.map_err(|_| ToolError::NotReady)?;
        Ok(DescribeReply {
            schema: SchemaId,
            project: about.project,
            display_name: about.display_name,
            access: about.access,
            access_limited_by: about.access_limited_by,
            modules: about.modules,
            tools: self.served().to_vec(),
            snapshot: indexed.snapshot,
        })
    }

    /// The findings of the code reader for one question: the snapshot's text and the question,
    /// redacted again with the manifest's rules, in one request; what comes back is parsed into
    /// findings that passed `safe_text` and a check against the request itself. A reader that
    /// fails is one fixed error and never findings.
    async fn ask_code(&self, args: AskCodeArgs) -> Result<AskCodeReply, ToolError> {
        let about = self.about()?;
        let reader = self.reader.as_ref().ok_or(ToolError::UnknownTool)?;
        let files = about.files.clone().ok_or(ToolError::NotReady)?;
        let question = redacted_question(args.question.as_str(), &about.redactor)
            .map_err(|_| ToolError::InvalidArguments)?;
        let (repo, modules, max_bytes) = (
            about.repo.clone(),
            about.modules.clone(),
            reader.client.max_bytes(),
        );
        let question = question.as_str().to_owned();
        // reading the snapshot and building what to check against is blocking work
        let (user, corpus) =
            tokio::task::spawn_blocking(move || -> Result<(String, Corpus), RequestError> {
                let snapshot = Snapshot::read(&repo, &files, max_bytes)?;
                let nonce = ask_code::nonce()?;
                Ok((
                    snapshot.user_message(&nonce, &modules, &question),
                    Corpus::of(&snapshot),
                ))
            })
            .await
            .map_err(|_| ToolError::ReaderFailed)?
            .map_err(|err| match err {
                RequestError::TooLarge => {
                    reader.note(Some(
                        "ask_code refused: the snapshot is larger than PROBE_CODE_READER_MAX_KB".to_owned(),
                    ));
                    ToolError::SnapshotTooLarge
                }
                RequestError::Unreadable => {
                    reader.note(Some("ask_code failed: the snapshot can't be read".to_owned()));
                    ToolError::ReaderFailed
                }
            })?;
        let answered = reader
            .client
            .read(ask_code::PROMPT.to_owned(), user)
            .await
            .map_err(|failure: ReadFailure| {
                reader.note(Some(format!("the code reader gave no answer ({failure})")));
                ToolError::ReaderFailed
            })?;
        let found = ask_code::findings(&answered.content, &corpus, &about.redactor, &about.modules).map_err(
            |_| {
                reader.note(Some(
                    "the code reader gave no answer (its reply isn't the findings asked for)".to_owned(),
                ));
                ToolError::ReaderFailed
            },
        )?;
        let cost_usd = Usd::new(answered.cost_usd).ok_or(ToolError::ReaderFailed)?;
        let indexed = self.index.snapshot().await.map_err(|_| ToolError::NotReady)?;
        reader.note(None);
        Ok(AskCodeReply {
            schema: SchemaId,
            as_of: indexed.snapshot.indexed_at,
            findings: found.findings,
            withheld: found.withheld,
            cost_usd,
            prompt_version: ask_code::PROMPT_VERSION,
        })
    }

    async fn progress(&self, args: ProgressArgs) -> Result<ProgressReply, ToolError> {
        args.check_module(&self.about()?.modules)
            .map_err(|_| ToolError::UnknownModule)?;
        let limit = usize::from(args.limit.map_or(Limit::MAX, Limit::get));
        let items = self
            .facts(|fact| {
                args.since
                    .is_none_or(|since| fact.updated_at.date_naive() >= since)
                    && args.module.as_ref().is_none_or(|m| &fact.module == m)
            })
            .await?
            .into_iter()
            .take(limit)
            .collect();
        Ok(ProgressReply {
            schema: SchemaId,
            as_of: self.as_of().await?,
            items,
        })
    }

    async fn tested(&self, args: TestedArgs) -> Result<TestedReply, ToolError> {
        args.check_module(&self.about()?.modules)
            .map_err(|_| ToolError::UnknownModule)?;
        let modules = self
            .index
            .module_stats()
            .await
            .map_err(|_| ToolError::NotReady)?
            .into_iter()
            .filter(|stats| args.module.as_ref().is_none_or(|m| &stats.module == m))
            .collect();
        Ok(TestedReply {
            schema: SchemaId,
            as_of: self.as_of().await?,
            modules,
        })
    }

    async fn blocked(&self, args: BlockedArgs) -> Result<BlockedReply, ToolError> {
        let items = self
            .facts(|fact| {
                fact.status == Status::Blocked
                    && args
                        .since
                        .is_none_or(|since| fact.updated_at.date_naive() >= since)
            })
            .await?
            .into_iter()
            .take(usize::from(Limit::MAX))
            .collect();
        Ok(BlockedReply {
            schema: SchemaId,
            as_of: self.as_of().await?,
            items,
        })
    }

    /// Every module's commits and merged pull requests since the date (or the one module's), with
    /// up to [`NOTABLE`] titles each; a date whose midnight is before the window the metadata covers
    /// is refused, since a count of part of the period, or part of its first day, would be wrong.
    async fn changes(&self, args: ChangesArgs) -> Result<ChangesReply, ToolError> {
        let modules = self.about()?.modules;
        args.check_module(&modules)
            .map_err(|_| ToolError::UnknownModule)?;
        let as_of = self.as_of().await?;
        let since = args.since.and_time(NaiveTime::MIN).and_utc();
        // to the instant: the window starts at `as_of`'s time of day, so the date 90 days back is
        // whole in it only when `as_of` is at midnight
        if since < as_of - crate::sync::WINDOW {
            return Err(ToolError::TooOld);
        }
        let counts = self
            .index
            .changes_since(since)
            .await
            .map_err(|_| ToolError::NotReady)?;
        let mut summaries = Vec::new();
        for module in modules
            .into_iter()
            .filter(|m| args.module.as_ref().is_none_or(|wanted| wanted == m))
        {
            let (commits, merged_prs) = counts
                .iter()
                .find(|(m, _, _)| m == &module)
                .map_or((0, 0), |&(_, commits, merged)| (commits, merged));
            let notable = self
                .index
                .notable_since(since, &module, NOTABLE)
                .await
                .map_err(|_| ToolError::NotReady)?;
            summaries.push(ChangeSummary {
                module,
                commits,
                merged_prs,
                notable,
            });
        }
        Ok(ChangesReply {
            schema: SchemaId,
            as_of,
            modules: summaries,
        })
    }

    async fn facts(&self, keep: impl Fn(&Fact) -> bool) -> Result<Vec<Fact>, ToolError> {
        Ok(self
            .index
            .facts()
            .await
            .map_err(|_| ToolError::NotReady)?
            .into_iter()
            .filter(keep)
            .collect())
    }

    /// The time the metadata was fetched: the facts are no fresher than that.
    async fn as_of(&self) -> Result<chrono::DateTime<chrono::Utc>, ToolError> {
        Ok(self
            .index
            .snapshot()
            .await
            .map_err(|_| ToolError::NotReady)?
            .snapshot
            .meta_at)
    }
}

/// Missing arguments are an empty object; anything that doesn't fit the schema is refused.
fn parse<T: DeserializeOwned>(arguments: serde_json::Value) -> Result<T, ToolError> {
    let arguments = match arguments {
        serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
        other => other,
    };
    serde_json::from_value(arguments).map_err(|_| ToolError::InvalidArguments)
}
