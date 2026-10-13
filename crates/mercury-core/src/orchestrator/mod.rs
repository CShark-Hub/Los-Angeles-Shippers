//! The orchestrator: one question through an explicit state machine, from its project to the
//! delivered answer. Every transition is an audit row, written before its event is sent; nothing a
//! model wrote reaches `answer` before the output checks; a judge that fails hands its stage to the
//! stage's rules, never to a pass; and an audit row that can't be written ends the question with
//! `internal`, with nothing delivered.
//!
//! `Received → Project → Classified → Authorized → Planned → ToolsChecked → ProbesCalled →
//! DataChecked → Composed → OutputChecked → Delivered`, ending in `Refused`, `Clarify` or `Failed`
//! from any state.

pub mod budget;
pub mod build;
pub mod compose;
pub mod facts;
pub mod judging;
pub mod plan;
mod texts;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mercury_judge::stages::{
    self, InputContext, PlannedCall, RuleContext, StageError, StageVerdicts, ToolModule,
};
use mercury_judge::{
    Catalogue, CatalogueError, ChatClient, ChatError, Completion, Format, Judge, Message, Prompt,
};
use mercury_schema::api::{
    Answer, ClarifyAnswer, ErrorCode, Flag, QuestionStatus, Refusal, RefusalCode, SignoffReason, Usd,
};
use mercury_schema::config::{BackendName, JudgeKind, ModelsConfig};
use mercury_schema::events::{
    Clarify, ClarifyKind, Classified, ComposerMode, Composing, Done, Event, EventError, Judged, Plan,
    ProjectPicked, ProjectSource, Refused, ToolCall, ToolChecked, ToolResult,
};
use mercury_schema::facts::Ratio;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{
    AccessMode, FieldPointer, JudgeMode, ModuleName, ProjectId, QuestionClass, QuestionId, RedactedQuestion,
    Redactor, Role, ToolName, Verdict,
};
use regex::Regex;
use serde_json::Value;

use crate::audit::AuditError;
use crate::config::Config;
use crate::db::{self, Db};
use crate::identity::role_name;
use crate::policy::{self, Caller, Decision, EffectivePolicy, Scope};
use crate::probes::{ProbeError, ProbeStatus, ToolReply};
use crate::questions::{self, Emitter};

use self::facts::{Reply, Shown, Unit};
use self::judging::Judging;

/// The input checks that refuse, and the refusal each gives.
const RISKS: [(&str, RefusalCode); 5] = [
    ("asks_for_code", RefusalCode::AsksForCode),
    ("asks_to_evaluate_people", RefusalCode::AsksToEvaluatePeople),
    ("asks_for_client_commitment", RefusalCode::AsksForClientCommitment),
    ("asks_for_confidential", RefusalCode::Confidential),
    (
        "contains_agent_instruction",
        RefusalCode::ContainsAgentInstruction,
    ),
];

/// What a question with no data at all is answered.
pub const NOTHING_FOUND: &str = "The project's data has nothing on this yet.";

/// A question as the API accepted it: its row exists and its `accepted` event is written.
#[derive(Clone)]
pub struct Asked {
    pub id: QuestionId,
    pub user_id: String,
    pub role: Role,
    /// Every project the caller is granted, the pseudo-project `portfolio` included.
    pub granted: Vec<ProjectId>,
    pub text: String,
    pub project: Option<ProjectId>,
    pub clarify: Option<ClarifyAnswer>,
}

/// Leaves the text out, so a stray `{:?}` doesn't log it.
impl std::fmt::Debug for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asked")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("granted", &self.granted)
            .field("project", &self.project)
            .field("clarify", &self.clarify)
            .finish_non_exhaustive()
    }
}

/// How a question ended, or that it waits for an approver; its row says the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Delivered,
    /// Held for sign-off: an approver's decision ends it.
    PendingSignoff,
    Refused(RefusalCode),
    Clarify,
    Failed(ErrorCode),
}

impl Ending {
    pub fn status(self) -> QuestionStatus {
        match self {
            Ending::Delivered => QuestionStatus::Delivered,
            Ending::PendingSignoff => QuestionStatus::PendingSignoff,
            Ending::Refused(_) => QuestionStatus::Refused,
            Ending::Clarify => QuestionStatus::Clarify,
            Ending::Failed(_) => QuestionStatus::Failed,
        }
    }
}

/// What the core knows of a project's probe.
#[derive(Debug, Clone)]
pub struct Registered {
    pub access: AccessMode,
    pub status: ProbeStatus,
    /// The manifest the probe read, as it registered it.
    pub manifest: Option<ProbeManifest>,
    /// The tools it registered.
    pub tools: Vec<ToolName>,
}

/// The probes, as the orchestrator calls them.
#[async_trait]
pub trait ProbeAccess: Send + Sync {
    /// `None` until the project's probe has registered.
    async fn registered(&self, project: &ProjectId) -> Result<Option<Registered>, ProbeError>;
    async fn call(&self, project: &ProjectId, tool: ToolName, args: Value) -> Result<ToolReply, ProbeError>;
}

/// A chat model that writes: the planner or the composer.
#[async_trait]
pub trait Writer: Send + Sync {
    async fn write(&self, messages: &[Message], format: Format) -> Result<Completion, ChatError>;
}

#[async_trait]
impl Writer for ChatClient {
    async fn write(&self, messages: &[Message], format: Format) -> Result<Completion, ChatError> {
        self.complete(messages, format).await
    }
}

/// A model role with the prompt it answers.
#[derive(Clone)]
pub struct Model {
    pub writer: Arc<dyn Writer>,
    pub prompt: Prompt,
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// What an orchestrator is made of.
pub struct Parts {
    pub db: Db,
    pub config: Arc<Config>,
    /// Jev, then the chat judge; each stage's rules answer when it fails.
    pub judge: Arc<dyn Judge>,
    pub probes: Arc<dyn ProbeAccess>,
    /// Without a planner the tools' defaults are used; without a composer the template answers.
    pub planner: Option<Model>,
    pub composer: Option<Model>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("the configuration has no check catalogue, so no question can be judged")]
    NoCatalogue,
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error("{0}")]
    Model(String),
}

#[derive(Clone)]
pub struct Orchestrator {
    db: Db,
    config: Arc<Config>,
    judging: Judging,
    probes: Arc<dyn ProbeAccess>,
    planner: Option<Model>,
    composer: Option<Model>,
    clock: Clock,
}

impl Orchestrator {
    pub fn new(parts: Parts) -> Result<Self, BuildError> {
        let checks = parts.config.checks.clone().ok_or(BuildError::NoCatalogue)?;
        let catalogue = Catalogue::new(checks)?;
        Ok(Self {
            db: parts.db,
            config: parts.config,
            judging: Judging::new(parts.judge, Arc::new(catalogue)),
            probes: parts.probes,
            planner: parts.planner,
            composer: parts.composer,
            clock: Arc::new(db::now),
        })
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Takes the question to its end, every event through `events`, and records the rest in its
    /// row.
    pub async fn run(&self, asked: Asked, events: Emitter) -> Ending {
        let mut run = Run::new(self, asked, events);
        let mut state = State::Received;
        let ending = loop {
            let step = match state {
                State::Received => run.received().await,
                State::Project => run.classify().await,
                State::Classified => run.authorize().await,
                State::Authorized => run.plan().await,
                State::Planned => run.check_tools().await,
                State::ToolsChecked => run.call_probes().await,
                State::ProbesCalled => run.check_data().await,
                State::DataChecked => run.compose().await,
                State::Composed => run.check_output().await,
                State::OutputChecked => run.deliver().await,
            };
            match step {
                Ok(Next::To(next)) => state = next,
                Ok(Next::End(ending)) => break ending,
                Err(stop) => break run.fail(stop).await,
            }
        };
        run.finish(ending).await;
        ending
    }
}

/// Where a question stands: each state is entered once its transition's rows are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Received,
    Project,
    Classified,
    Authorized,
    Planned,
    ToolsChecked,
    ProbesCalled,
    DataChecked,
    Composed,
    OutputChecked,
}

enum Next {
    To(State),
    End(Ending),
}

/// Why a question failed after it started.
#[derive(Debug, thiserror::Error)]
enum Stop {
    #[error("the audit row can't be written: {0}")]
    Audit(#[from] AuditError),
    #[error("no judge answered: {0}")]
    Judge(#[from] StageError),
    #[error("{0}")]
    Probe(String),
    #[error("the database failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
    #[error("the question went over its budget")]
    Budget,
}

impl Stop {
    fn code(&self) -> ErrorCode {
        match self {
            Stop::Audit(_) | Stop::Db(_) | Stop::Internal(_) => ErrorCode::Internal,
            Stop::Judge(_) => ErrorCode::JudgeUnavailable,
            Stop::Probe(_) => ErrorCode::ProbeUnavailable,
            Stop::Invalid(_) => ErrorCode::InvalidRequest,
            Stop::Budget => ErrorCode::BudgetExceeded,
        }
    }

    /// What the person is told; never the error's own words, which may name internals.
    fn message(&self) -> String {
        match self {
            Stop::Audit(_) | Stop::Db(_) | Stop::Internal(_) => {
                "The server could not finish the question.".to_owned()
            }
            Stop::Judge(_) => "The judge that checks every answer is unavailable.".to_owned(),
            Stop::Probe(message) | Stop::Invalid(message) => message.clone(),
            Stop::Budget => "The question reached its spending limit, so it was stopped.".to_owned(),
        }
    }
}

/// A question as it goes: what each state decided, for the ones after it.
struct Run<'a> {
    o: &'a Orchestrator,
    asked: Asked,
    started: Instant,
    /// Writes each event's audit row, brings the question's row up to date and wakes the streams.
    events: Emitter,
    /// The `seq` of the last row written.
    last_seq: Option<u64>,
    /// The caller's projects that exist here; `portfolio` isn't one.
    candidates: Vec<ProjectId>,
    project: Option<ProjectId>,
    picked: Option<(f64, ProjectSource)>,
    registered: Option<Registered>,
    manifest: Option<ProbeManifest>,
    policy: Option<EffectivePolicy>,
    /// The question as anything outside the core reads it: redacted with the project's rules, or
    /// with every candidate project's until the project is settled.
    text: String,
    /// The project's redaction rules, once the project is settled.
    redactor: Option<Redactor>,
    /// The client names the output's rule looks for: the global rules', then the project's too.
    client_patterns: Vec<Regex>,
    class: Option<QuestionClass>,
    class_fixed: bool,
    /// The tools the class calls with the probe it has.
    fixed: Vec<ToolName>,
    calls: Vec<ToolCall>,
    planned_by_model: bool,
    /// Whether the plan holds a call to the code reader, whose question the tool stage still checks
    /// when no model filled the plan.
    reads_code: bool,
    replies: Vec<Reply>,
    missing: Vec<ToolName>,
    units: Vec<Unit>,
    sentences: Vec<String>,
    composed_by_model: bool,
    regenerated: bool,
    kept: Vec<String>,
    withheld_note: bool,
    modes: Vec<JudgeMode>,
    cost: f64,
    prompt_version: Option<String>,
    answer_text: Option<String>,
    /// Why the answer waits for an approver, once something holds it.
    signoff: Option<SignoffReason>,
    /// The sentences held for the approver, each with the check that put it in its review band.
    flags: Vec<Flag>,
}

type Step = Result<Next, Stop>;

impl<'a> Run<'a> {
    fn new(o: &'a Orchestrator, asked: Asked, events: Emitter) -> Self {
        let candidates: Vec<ProjectId> = asked
            .granted
            .iter()
            .filter(|p| p.as_str() != mercury_schema::config::PORTFOLIO)
            .filter(|p| o.config.projects.projects.contains_key(*p))
            .cloned()
            .collect();
        Self {
            o,
            asked,
            started: Instant::now(),
            events,
            last_seq: None,
            candidates,
            project: None,
            picked: None,
            registered: None,
            manifest: None,
            policy: None,
            text: String::new(),
            redactor: None,
            client_patterns: Vec::new(),
            class: None,
            class_fixed: false,
            fixed: Vec::new(),
            calls: Vec::new(),
            planned_by_model: false,
            reads_code: false,
            replies: Vec::new(),
            missing: Vec::new(),
            units: Vec::new(),
            sentences: Vec::new(),
            composed_by_model: false,
            regenerated: false,
            kept: Vec::new(),
            withheld_note: false,
            modes: Vec::new(),
            cost: 0.0,
            prompt_version: None,
            answer_text: None,
            signoff: None,
            flags: Vec::new(),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.o.clock)()
    }

    async fn emit(&mut self, event: Event) -> Result<(), Stop> {
        let recorded = self.events.emit(event).await.map_err(Stop::Audit)?;
        self.last_seq = Some(recorded.seq());
        Ok(())
    }

    async fn refuse(&mut self, refusal: Refusal) -> Step {
        let code = refusal.code;
        let refused = Refused::new(refusal, self.cost_so_far(), self.judged_so_far());
        self.emit(Event::Refused(refused)).await?;
        Ok(Next::End(Ending::Refused(code)))
    }

    async fn clarify(&mut self, clarify: Clarify) -> Step {
        let clarify = Clarify {
            cost_usd: self.cost_so_far(),
            judge_mode: self.judged_so_far(),
            ..clarify
        };
        self.emit(Event::Clarify(clarify)).await?;
        Ok(Next::End(Ending::Clarify))
    }

    /// What the question has cost so far.
    fn cost_so_far(&self) -> Usd {
        Usd::new(self.cost).unwrap_or_default()
    }

    /// The weakest judge any stage had, or `None` before any judge answered.
    fn judged_so_far(&self) -> Option<JudgeMode> {
        (!self.modes.is_empty()).then(|| self.judge_mode())
    }

    /// A stage decided: its judge joins the question's modes, the question's row says the weakest
    /// so far (an ending at a restart reads it there), and the call is counted.
    async fn judged_by(&mut self, decided: &StageVerdicts) -> Result<(), Stop> {
        let before = self.judged_so_far();
        self.modes.push(decided.outcome.mode);
        let weakest = self.judge_mode();
        if before != Some(weakest) {
            let (mode, id) = (mode_name(weakest), self.asked.id.as_str());
            sqlx::query!("UPDATE questions SET judge_mode = ? WHERE id = ?", mode, id)
                .execute(self.o.db.pool())
                .await?;
        }
        let backend = judge_backend(&self.o.config.models, decided.outcome.mode);
        self.charge(decided.outcome.cost_usd, backend).await
    }

    /// Counts a paid call in `spend` and in the question's row, then against the question's own
    /// cap: the stage that goes over it is the question's last. A call that reports no cost fails
    /// closed on a paid backend: it counts as what is left of the question's cap, and once nothing
    /// is left it is over the cap. On a model server of one's own it costs nothing, since no
    /// provider bills it. `backend` is the one that answered; `None` for the rules.
    async fn charge(&mut self, reported: Option<f64>, backend: Option<BackendName>) -> Result<(), Stop> {
        let cap = self.o.config.models.budgets.per_question_usd.get();
        let (cost, total) = match reported {
            Some(cost) if cost <= 0.0 => return Ok(()),
            Some(cost) => (cost, self.cost + cost),
            None if backend == Some(BackendName::OpenaiCompatible) => return Ok(()),
            None => {
                let left = cap - self.cost;
                if left <= 0.0 {
                    tracing::warn!(
                        "a model call reported no cost, and the question has nothing left to spend"
                    );
                    return Err(Stop::Budget);
                }
                tracing::warn!(
                    "a model call reported no cost; it counts as the rest of the question's budget"
                );
                (left, cap)
            }
        };
        self.cost = total;
        let at = self.now();
        budget::charge(
            &self.o.db,
            self.asked.id.as_str(),
            &self.asked.user_id,
            cost,
            self.cost,
            at,
        )
        .await?;
        if self.cost > cap {
            return Err(Stop::Budget);
        }
        Ok(())
    }

    fn rules(&self) -> RuleContext {
        RuleContext {
            client_patterns: self.client_patterns.clone(),
            fact_ids: self.units.iter().map(|u| u.id.clone()).collect(),
            projects: self.candidates.iter().map(|p| p.as_str().to_owned()).collect(),
        }
    }

    fn modules(&self) -> Vec<ModuleName> {
        self.manifest.as_ref().map(plan::modules).unwrap_or_default()
    }

    /// The modules the tool stage's judge reads, each with its labels: the names a person may use
    /// for it, without a module or a label the project's rules would change (a label can name a
    /// client), without a label that isn't shaped like one, and without `Other`, which no question
    /// names.
    fn modules_for_the_judge(&self) -> Vec<ToolModule> {
        let names = self.manifest.as_ref().map(plan::names).unwrap_or_default();
        let shown = match &self.redactor {
            Some(redactor) => plan::shown_to_models(&names, redactor),
            None => Vec::new(),
        };
        shown
            .into_iter()
            .filter(|(module, _)| module.as_str() != ModuleName::OTHER)
            .map(|(module, labels)| ToolModule {
                name: module.as_str().to_owned(),
                labels,
            })
            .collect()
    }

    /// Settles the project and what depends on it: the probe, the manifest, the redactor.
    async fn settle(&mut self, project: ProjectId, p: f64, source: ProjectSource) -> Result<(), Stop> {
        let registered = self
            .o
            .probes
            .registered(&project)
            .await
            .map_err(|err| match err {
                ProbeError::Db(err) => Stop::Db(err),
                other => Stop::Probe(format!("The project's probe can't be read: {other}.")),
            })?;
        let manifest = crate::config::manifest_of(
            &self.o.config.projects,
            &project,
            registered.as_ref().and_then(|r| r.manifest.as_ref()),
        );
        if let Some(manifest) = &manifest {
            // a rule that can't be built ends the question: never fewer rules than the project has
            let redactor = policy::project_redactor(&self.o.config.policies, manifest)
                .map_err(|_| Stop::Internal("the project's redaction rules can't be built".to_owned()))?;
            self.client_patterns = client_patterns(&self.o.config, Some(manifest))?;
            self.text = redactor.apply(&self.asked.text).into_owned();
            self.redactor = Some(redactor);
        }
        self.registered = registered;
        self.manifest = manifest;
        self.picked = Some((p, source));
        self.project = Some(project);
        Ok(())
    }

    async fn emit_project(&mut self) -> Result<(), Stop> {
        let (Some(project), Some((p, source))) = (self.project.clone(), self.picked) else {
            return Ok(());
        };
        self.emit(Event::Project(ProjectPicked {
            project,
            p: ratio(p),
            source,
        }))
        .await
    }

    /// Before anything leaves the core: the question redacted with the global rules and the rules
    /// of every project the caller may ask about, since which one it is about isn't known yet, and
    /// the global client names for the output's rule. Rules that can't be built end the question.
    async fn prepare(&mut self) -> Result<(), Stop> {
        let mut rules = self.o.config.policies.global.redact.clone();
        for project in self.candidates.clone() {
            let registered = self
                .o
                .probes
                .registered(&project)
                .await
                .map_err(|err| match err {
                    ProbeError::Db(err) => Stop::Db(err),
                    other => Stop::Probe(format!("The project's probe can't be read: {other}.")),
                })?;
            let manifest = crate::config::manifest_of(
                &self.o.config.projects,
                &project,
                registered.as_ref().and_then(|r| r.manifest.as_ref()),
            );
            if let Some(manifest) = manifest {
                rules.extend(manifest.redact);
            }
        }
        let redactor = Redactor::new(&rules)
            .map_err(|_| Stop::Internal("the redaction rules can't be built".to_owned()))?;
        self.text = redactor.apply(&self.asked.text).into_owned();
        self.client_patterns = client_patterns(&self.o.config, None)?;
        Ok(())
    }

    /// `Received`: the project when code can tell it, a clarifying answer applied.
    async fn received(&mut self) -> Step {
        self.prepare().await?;
        let mut named = self.asked.project.clone();
        if let Some(answer) = self.asked.clarify.clone() {
            match self.clarified(&answer).await? {
                Some((ClarifyKind::Project, option, _)) => named = ProjectId::parse(&option).ok(),
                Some((ClarifyKind::Class, option, asked_about)) => {
                    self.class = class_of(&option);
                    self.class_fixed = self.class.is_some();
                    // the class answers a question that had its project already, and keeps it
                    if named.is_none() {
                        named = asked_about;
                    }
                }
                None => {
                    return Err(Stop::Invalid(
                        "The answer to the clarifying question doesn't match the question it answers."
                            .to_owned(),
                    ));
                }
            }
        }
        if let Some(project) = named {
            if !self.candidates.contains(&project) {
                return self
                    .refuse(texts::by_policy(&policy::DenyReason::ForbiddenProject {
                        granted: self.candidates.clone(),
                    }))
                    .await;
            }
            self.settle(project, 1.0, ProjectSource::Explicit).await?;
        } else if self.candidates.is_empty() {
            return self
                .refuse(texts::by_policy(&policy::DenyReason::ForbiddenProject {
                    granted: Vec::new(),
                }))
                .await;
        } else if let Some(project) = self.only_or_named() {
            // one project is no choice, and a name in the question needs no judge
            self.settle(project, 1.0, ProjectSource::Judge).await?;
        }
        self.emit_project().await?;
        Ok(Next::To(State::Project))
    }

    /// The caller's one project, or the one the question names by its id or an alias.
    fn only_or_named(&self) -> Option<ProjectId> {
        if let [only] = self.candidates.as_slice() {
            return Some(only.clone());
        }
        let named: Vec<&ProjectId> = self
            .candidates
            .iter()
            .filter(|project| {
                let aliases = self
                    .o
                    .config
                    .projects
                    .projects
                    .get(*project)
                    .map(|config| config.aliases.clone())
                    .unwrap_or_default();
                std::iter::once(project.as_str().to_owned())
                    .chain(aliases)
                    .any(|name| plan::mentions(&self.asked.text, &name))
            })
            .collect();
        match named.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    }

    /// The kind and option of a clarifying answer, if it answers a clarify of this caller's, and
    /// the project the question it answers was about.
    async fn clarified(
        &self,
        answer: &ClarifyAnswer,
    ) -> Result<Option<(ClarifyKind, String, Option<ProjectId>)>, Stop> {
        let of = answer.of.as_str();
        let row = sqlx::query!("SELECT user_id, project FROM questions WHERE id = ?", of)
            .fetch_optional(self.o.db.pool())
            .await?;
        let Some(row) = row.filter(|row| row.user_id == self.asked.user_id) else {
            return Ok(None);
        };
        let payload = sqlx::query_scalar!(
            "SELECT payload_json FROM audit_events WHERE question_id = ? AND type = 'clarify' ORDER BY seq DESC LIMIT 1",
            of
        )
        .fetch_optional(self.o.db.pool())
        .await?
        .flatten();
        let Some(clarify) = payload.and_then(|json| serde_json::from_str::<Clarify>(&json).ok()) else {
            return Ok(None);
        };
        let known = clarify.options.iter().any(|option| option.id == answer.option);
        let asked_about = row.project.as_deref().and_then(|p| ProjectId::parse(p).ok());
        Ok(known.then(|| (clarify.kind, answer.option.clone(), asked_about)))
    }

    /// `Project → Classified`: one request for the project (when it is open), the class and the
    /// four input checks.
    async fn classify(&mut self) -> Step {
        let projects: Vec<String> = if self.project.is_none() {
            self.candidates.iter().map(|p| p.as_str().to_owned()).collect()
        } else {
            Vec::new()
        };
        let ctx = InputContext {
            text: &self.text,
            role: role_name(self.asked.role),
            project: self.project.as_ref().map(ProjectId::as_str),
            projects: &projects,
        };
        let mut stage = stages::input_batch(self.o.judging.catalogue(), &ctx);
        if self.class_fixed {
            stage.batch.questions.shift_remove("class");
            stage.keys.shift_remove("class");
        }
        let decided = self.o.judging.decide(&stage, &self.rules()).await?;
        self.judged_by(&decided).await?;

        if self.project.is_none()
            && decided.verdicts.get("project") == Some(&Verdict::Pass)
            && let Some((choice, p)) = judging::pick(&decided, "project")
            && let Some(project) = self.candidates.iter().find(|c| c.as_str() == choice).cloned()
        {
            self.settle(project, p, ProjectSource::Judge).await?;
            self.emit_project().await?;
        }

        let (class, p) = if self.class_fixed {
            (self.class, 1.0)
        } else {
            match judging::pick(&decided, "class") {
                Some((name, p)) => (class_of(&name), p),
                None => (None, 0.0),
            }
        };
        let class_unsure = !self.class_fixed && decided.verdicts.get("class") != Some(&Verdict::Pass);
        let risk_ids: Vec<&str> = RISKS.iter().map(|(id, _)| *id).collect();
        let checks = judging::results(&stage, &decided, &risk_ids);
        let Some(class) = class else {
            return Err(Stop::Judge(StageError::Judge(
                mercury_judge::JudgeError::MissingAnswer("class".to_owned()),
            )));
        };
        self.class = Some(class);
        self.emit(Event::Classified(Classified {
            class,
            p: ratio(p),
            checks,
        }))
        .await?;

        // a risk refuses before anything else is asked of the person
        for (id, code) in RISKS {
            if decided.verdicts.get(id).is_some_and(|v| *v != Verdict::Pass) {
                let modules = self.modules();
                return self.refuse(texts::by_check(code, &modules)).await;
            }
        }
        let Some(project) = self.project.clone() else {
            let options = self
                .candidates
                .iter()
                .map(|p| {
                    let label = self
                        .o
                        .config
                        .projects
                        .projects
                        .get(p)
                        .map_or_else(|| p.as_str().to_owned(), |c| c.display_name.clone());
                    (p.clone(), label)
                })
                .collect();
            return self.clarify(texts::which_project(options)).await;
        };
        if class_unsure {
            if decided.outcome.mode == JudgeMode::Rules {
                let modules = self.modules();
                return self
                    .refuse(texts::by_check(RefusalCode::StrictMode, &modules))
                    .await;
            }
            let allowed = match &self.manifest {
                Some(manifest) => {
                    EffectivePolicy::for_project(&self.o.config.policies, self.asked.role, manifest).allowed
                }
                None => QuestionClass::PROJECT.to_vec(),
            };
            return self.clarify(texts::which_class(project.as_str(), &allowed)).await;
        }
        Ok(Next::To(State::Classified))
    }

    /// `Classified → Authorized`: the policy decides; its refusal says what can be asked.
    async fn authorize(&mut self) -> Step {
        let (Some(project), Some(class)) = (self.project.clone(), self.class) else {
            return Err(Stop::Internal("the question has no project or class".to_owned()));
        };
        let caller = Caller {
            role: self.asked.role,
            granted: &self.asked.granted,
        };
        if class == QuestionClass::Portfolio {
            let decision = policy::authorize(&self.o.config.policies, caller, Scope::Portfolio, class);
            if let Decision::Deny(reason) = decision {
                return self.refuse(texts::by_policy(&reason)).await;
            }
            let candidates = self.candidates.clone();
            return self.refuse(texts::portfolio(&candidates)).await;
        }
        let Some(manifest) = self.manifest.clone() else {
            return Err(Stop::Probe(
                "The project's probe hasn't registered yet, and there is no copy of its manifest.".to_owned(),
            ));
        };
        let scope = Scope::Project {
            project: &project,
            manifest: &manifest,
        };
        match policy::authorize(&self.o.config.policies, caller, scope, class) {
            Decision::Deny(reason) => self.refuse(texts::by_policy(&reason)).await,
            decision => {
                // a class held for sign-off is answered like any other, and the answer waits for
                // an approver instead of being delivered
                if let Decision::NeedsSignoff(reason) = decision {
                    self.signoff = Some(reason);
                }
                self.policy = Some(EffectivePolicy::for_project(
                    &self.o.config.policies,
                    self.asked.role,
                    &manifest,
                ));
                Ok(Next::To(State::Authorized))
            }
        }
    }

    /// `Authorized → Planned`: the class's fixed tools; the planner fills only their arguments. An
    /// `open` question has none to fill, so the planner isn't asked: it takes every fact tool the
    /// probe serves with its defaults, and the code reader when the project has one.
    async fn plan(&mut self) -> Step {
        let Some(class) = self.class else {
            return Err(Stop::Internal("the question has no class".to_owned()));
        };
        let today = self.now().date_naive();
        let names = self.manifest.as_ref().map(plan::names).unwrap_or_default();
        // what a model reads of them: no module or label the project's rules would change, and no
        // label that isn't shaped like one
        let shown = match &self.redactor {
            Some(redactor) => plan::shown_to_models(&names, redactor),
            None => Vec::new(),
        };
        let mut filled = None;
        if class != QuestionClass::Open
            && let Some(planner) = self.o.planner.clone()
        {
            let prompt = plan::prompt(&planner.prompt.text, today, &shown, &self.text);
            match planner.writer.write(&[Message::user(prompt)], Format::Json).await {
                Ok(completion) => {
                    let backend = self.o.config.models.roles.planner.backend;
                    self.charge(completion.cost_usd, Some(backend)).await?;
                    match plan::parse(&completion.content, today, &names) {
                        Some(parsed) => filled = Some(parsed),
                        None => tracing::warn!(
                            "the planner's answer isn't one JSON object; the defaults are used"
                        ),
                    }
                }
                Err(err) => tracing::warn!(error = %err, "the planner gave no answer; the defaults are used"),
            }
        }
        self.planned_by_model = filled.as_ref().is_some_and(|f| !f.is_empty());
        // without the planner's answer, code reads a module the question names outright; an open
        // question names none for the tools to take, and goes to the reader whole
        let filled = match class {
            QuestionClass::Open => plan::Filled::default(),
            _ => filled.unwrap_or_else(|| plan::Filled {
                module: plan::named_module(&self.asked.text, &names),
                ..plan::Filled::default()
            }),
        };
        let served = self
            .registered
            .as_ref()
            .map(|r| r.tools.clone())
            .unwrap_or_default();
        let access = self
            .registered
            .as_ref()
            .map_or(AccessMode::Metadata, |r| r.access);
        self.fixed = plan::tools_for(class, &served, access);
        let question = if self.fixed.contains(&ToolName::AskCode) {
            self.question_for_the_reader()
        } else {
            None
        };
        if question.is_none() {
            self.fixed.retain(|tool| *tool != ToolName::AskCode);
        }
        let filled = plan::filled_for(class, &self.fixed, filled, today);
        self.calls = plan::calls(&self.fixed, &filled, today, question.as_ref());
        self.reads_code = self.calls.iter().any(|call| call.tool() == ToolName::AskCode);
        if self.calls.is_empty() {
            return Err(Stop::Internal("no probe tool answers the class".to_owned()));
        }
        self.emit(Event::Plan(Plan {
            calls: self.calls.clone(),
        }))
        .await?;
        Ok(Next::To(State::Planned))
    }

    /// The question as the code reader takes it, when it may be asked. What the reader reports is
    /// repository text, which a policy that hides titles from models or shows aggregates only keeps
    /// from them too, so such a policy plans no reader call; and a question that doesn't fit once
    /// redacted is never shortened.
    fn question_for_the_reader(&self) -> Option<RedactedQuestion> {
        let policy = self.policy.as_ref()?;
        if !policy.show_titles || policy.aggregates_only {
            return None;
        }
        plan::code_question(&self.asked.text, self.redactor.as_ref()?)
    }

    /// `Planned → ToolsChecked`: the scope and the class by code, the arguments by the judge, in
    /// one request. Arguments that come from code alone aren't asked about.
    async fn check_tools(&mut self) -> Step {
        let planned: Vec<PlannedCall> = self
            .calls
            .iter()
            .map(|call| PlannedCall {
                tool: call.tool().as_str().to_owned(),
                args: plan::arguments(call),
            })
            .collect();
        let modules = self.modules_for_the_judge();
        let mut stage = stages::tool_batch(self.o.judging.catalogue(), &self.text, &planned, &modules);
        // arguments that come from code alone aren't asked about; the reader's question is one of
        // them, made from the person's question by redaction, which the input stage read
        if !self.planned_by_model {
            stage.batch.questions.shift_remove("args_match_question");
            stage.keys.shift_remove("args_match_question");
        }
        let decided = self.o.judging.decide(&stage, &self.rules()).await?;
        self.judged_by(&decided).await?;
        // with the rules alone at this stage no model has judged the plan, and the rules can't tell
        // that the reader's call fits the question: no question reaches the reader that way
        let reader_unjudged = self.reads_code && decided.outcome.mode == JudgeMode::Rules;
        let in_scope = self.calls.iter().all(|call| self.fixed.contains(&call.tool()));
        let mut checks: Vec<_> = [
            judging::rule("scope", if in_scope { Verdict::Pass } else { Verdict::Block }),
            judging::rule("deny_class", Verdict::Pass),
            reader_unjudged
                .then(|| judging::rule("args_match_question", Verdict::Block))
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .collect();
        checks.extend(judging::results(
            &stage,
            &decided,
            &["args_match_question", "args_free_of_instructions"],
        ));
        for call in 0..self.calls.len() {
            self.emit(Event::ToolChecked(ToolChecked {
                call: u16::try_from(call).unwrap_or(u16::MAX),
                checks: checks.clone(),
            }))
            .await?;
        }
        if !in_scope {
            let modules = self.modules();
            return self
                .refuse(texts::by_check(RefusalCode::StrictMode, &modules))
                .await;
        }
        // a review refuses like a block: sign-off holds an answer for an approver, and at the tool
        // stage there is no answer yet to hold
        if decided
            .verdicts
            .get("args_free_of_instructions")
            .is_some_and(|v| *v != Verdict::Pass)
        {
            let modules = self.modules();
            return self
                .refuse(texts::by_check(RefusalCode::ContainsAgentInstruction, &modules))
                .await;
        }
        if reader_unjudged
            || decided
                .verdicts
                .get("args_match_question")
                .is_some_and(|v| *v != Verdict::Pass)
        {
            let modules = self.modules();
            return self
                .refuse(texts::by_check(RefusalCode::StrictMode, &modules))
                .await;
        }
        Ok(Next::To(State::ToolsChecked))
    }

    /// `ToolsChecked → ProbesCalled`: every call at once; a call that fails leaves its part out.
    async fn call_probes(&mut self) -> Step {
        let Some(project) = self.project.clone() else {
            return Err(Stop::Internal("the question has no project".to_owned()));
        };
        let mut tasks = tokio::task::JoinSet::new();
        for (index, call) in self.calls.iter().enumerate() {
            let probes = self.o.probes.clone();
            let (project, tool, args) = (project.clone(), call.tool(), plan::arguments(call));
            tasks.spawn(async move { (index, tool, probes.call(&project, tool, args).await) });
        }
        let mut results = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(result) => results.push(result),
                Err(err) => tracing::warn!(error = %err, "a probe call's task failed"),
            }
        }
        results.sort_by_key(|(index, ..)| *index);
        // The code reader's reported cost is a paid call of the question, counted before anything
        // below can end it: a call that failed, a reply that can't be redacted, no reply at all. So
        // it is in `done`, in the question's row and in both `spend` rows however the rest went, and
        // a question it takes over its cap ends `budget_exceeded` here, the cost kept. A reader
        // that failed or timed out reported nothing, so nothing is counted for it.
        for (_, _, result) in &results {
            if let Ok(ToolReply::AskCode(reply)) = result {
                // the prompt's version is the reader's own record, for whoever reads a finding later
                tracing::debug!(
                    question = %self.asked.id.as_str(),
                    prompt_version = reply.prompt_version,
                    "the code reader answered"
                );
                self.charge(Some(reply.cost_usd.get()), None).await?;
            }
        }
        // aggregates only means no titles, whatever the defaults say
        let show_titles = self
            .policy
            .as_ref()
            .is_none_or(|p| p.show_titles && !p.aggregates_only);
        for (index, tool, result) in results {
            match result {
                Ok(mut reply) => {
                    if !show_titles {
                        facts::withhold_titles(&mut reply);
                    }
                    // the core's rules on top of the probe's, before anything reads a text
                    let Some(redactor) = self.redactor.as_ref() else {
                        return Err(Stop::Internal(
                            "the project's redaction rules aren't settled".to_owned(),
                        ));
                    };
                    let (reply, withheld) = facts::redact(&reply, redactor).map_err(|err| {
                        Stop::Internal(format!("a reply can't hold its redacted texts: {err}"))
                    })?;
                    self.replies.push(Reply {
                        call: u16::try_from(index).unwrap_or(u16::MAX),
                        tool,
                        reply,
                        withheld: withheld
                            .iter()
                            .filter_map(|p| FieldPointer::parse(p).ok())
                            .collect(),
                    });
                }
                Err(ProbeError::Db(err)) => return Err(Stop::Db(err)),
                Err(err) => {
                    tracing::warn!(tool = tool.as_str(), error = %err, "a probe call failed; its part is left out");
                    self.missing.push(tool);
                }
            }
        }
        if self.replies.is_empty() {
            return Err(Stop::Probe(
                "The project's probe can't answer right now.".to_owned(),
            ));
        }
        Ok(Next::To(State::ProbesCalled))
    }

    /// `ProbesCalled → DataChecked`: every repository text in one request; a flagged one is
    /// withheld before the composer sees it, and each `tool_result` records what went.
    async fn check_data(&mut self) -> Step {
        let mut fields: Vec<(usize, String, String)> = Vec::new();
        for (r, reply) in self.replies.iter().enumerate() {
            for (pointer, text) in facts::texts(&reply.reply) {
                fields.push((r, pointer, text));
            }
        }
        // each reply's lowest probability that its texts are fine, where a judge read them
        let mut lowest: Vec<Option<Ratio>> = vec![None; self.replies.len()];
        if !fields.is_empty() {
            let texts: Vec<String> = fields.iter().map(|(_, _, text)| text.clone()).collect();
            let stage = stages::data_batch(self.o.judging.catalogue(), &texts);
            let decided = self.o.judging.decide(&stage, &self.rules()).await?;
            self.judged_by(&decided).await?;
            for (r, p_min) in lowest.iter_mut().enumerate() {
                let items: Vec<usize> = fields
                    .iter()
                    .enumerate()
                    .filter(|(_, (reply, ..))| *reply == r)
                    .map(|(i, _)| i)
                    .collect();
                *p_min = judging::p_min(&stage, &decided, &items, self.o.judging.catalogue());
            }
            let mut withhold: Vec<Vec<String>> = vec![Vec::new(); self.replies.len()];
            for (i, (r, pointer, _)) in fields.iter().enumerate() {
                if judging::strictest(&stage, &decided, Some(i)) != Verdict::Pass
                    && let Some(list) = withhold.get_mut(*r)
                {
                    list.push(pointer.clone());
                }
            }
            for (reply, pointers) in self.replies.iter_mut().zip(withhold) {
                reply.reply = facts::withhold(&reply.reply, &pointers)
                    .map_err(|err| Stop::Internal(format!("a reply can't hold what was withheld: {err}")))?;
                for pointer in pointers.iter().filter_map(|p| FieldPointer::parse(p).ok()) {
                    if !reply.withheld.contains(&pointer) {
                        reply.withheld.push(pointer);
                    }
                }
            }
        }
        let access = self
            .registered
            .as_ref()
            .map(|r| r.access)
            .or_else(|| self.manifest.as_ref().map(|m| m.access))
            .unwrap_or(AccessMode::Metadata);
        let results: Vec<ToolResult> = self
            .replies
            .iter()
            .zip(lowest)
            .map(|(reply, p_min)| ToolResult {
                call: reply.call,
                items: facts::items(&reply.reply),
                withheld_fields: reply.withheld.clone(),
                access,
                p_min,
            })
            .collect();
        for result in results {
            self.emit(Event::ToolResult(result)).await?;
        }
        let shown = Shown {
            people: self
                .policy
                .as_ref()
                .is_some_and(|p| p.show_people && !p.aggregates_only),
            evidence: self.policy.as_ref().is_some_and(|p| p.show_evidence_refs),
        };
        self.units = facts::units(&self.replies, shown, &self.modules());
        Ok(Next::To(State::DataChecked))
    }

    /// `DataChecked → Composed`: the composer writes from the units alone; without it, the
    /// template does.
    async fn compose(&mut self) -> Step {
        self.emit(Event::Composing(Composing {})).await?;
        self.sentences.clear();
        if self.units.is_empty() {
            return Ok(Next::To(State::Composed));
        }
        let facts_json = facts::facts_json(&self.units);
        let mut written = None;
        if let Some(composer) = self.o.composer.clone() {
            let prompt = compose::prompt(&composer.prompt.text, &facts_json, &self.text);
            match composer
                .writer
                .write(&[Message::user(prompt)], Format::Text)
                .await
            {
                Ok(completion) => {
                    let backend = self.o.config.models.roles.composer.backend;
                    self.charge(completion.cost_usd, Some(backend)).await?;
                    written = Some((completion.content, composer.prompt.version.clone()));
                }
                Err(err) => tracing::warn!(error = %err, "the composer gave no answer; the template answers"),
            }
        }
        let (text, version, by_model) = match written {
            Some((text, version)) => (text, version, true),
            None => (compose::template(&self.units), "template".to_owned(), false),
        };
        self.sentences = compose::within_words(compose::sentences(&text));
        self.composed_by_model = by_model;
        self.prompt_version = Some(version);
        Ok(Next::To(State::Composed))
    }

    /// `Composed → OutputChecked`: the citation rule, then every sentence × every output check in
    /// one request; a block strips the sentence, and so does a review, unless the policy holds
    /// review bands for sign-off and someone besides the asker approves the project: then the
    /// sentence stays and the answer waits for them. More than half stripped: the composer writes
    /// once more.
    async fn check_output(&mut self) -> Step {
        self.flags.clear();
        let hold_reviews = self.o.config.policies.global.hold_review_band_for_signoff
            && match &self.project {
                Some(project) => {
                    crate::approvals::has_approver(&self.o.db, project, &self.asked.user_id).await?
                }
                None => false,
            };
        let known: Vec<&str> = self.units.iter().map(|u| u.id.as_str()).collect();
        let total = self.sentences.len();
        // each sentence as the person will read it, with the ids it cites after it: the checks
        // read exactly the text the answer shows
        let shown: Vec<String> = self.sentences.iter().map(|s| compose::as_shown(s)).collect();
        let cited: Vec<String> = shown
            .iter()
            .filter(|sentence| {
                let ids = compose::citations(sentence);
                !compose::without_citations(sentence).is_empty()
                    && !ids.is_empty()
                    && ids.iter().all(|id| known.contains(&id.as_str()))
            })
            .cloned()
            .collect();
        let mut kept = Vec::new();
        let mut held = 0;
        let mut p_min = None;
        if !cited.is_empty() {
            let facts_json = facts::facts_json(&self.units);
            let stage = stages::output_batch(self.o.judging.catalogue(), &cited, &facts_json);
            let decided = self.o.judging.decide_output(&stage, &self.rules()).await?;
            self.judged_by(&decided).await?;
            // the sentences the person reads: delivered, or held for the approver
            let mut read = Vec::new();
            for (i, sentence) in cited.iter().enumerate() {
                match judging::strictest(&stage, &decided, Some(i)) {
                    Verdict::Pass => {
                        kept.push(sentence.clone());
                        read.push(i);
                    }
                    Verdict::Review if hold_reviews => {
                        let shown = compose::without_citations(sentence);
                        for (check, p) in judging::reviews(&stage, &decided, i) {
                            self.flags.push(Flag {
                                sentence: shown.clone(),
                                check,
                                p,
                            });
                        }
                        kept.push(sentence.clone());
                        read.push(i);
                        held += 1;
                    }
                    Verdict::Review | Verdict::Block => {}
                }
            }
            p_min = judging::p_min(&stage, &decided, &read, self.o.judging.catalogue());
        }
        let stripped = total - kept.len();
        self.emit(Event::Judged(Judged {
            sentences: u16::try_from(total).unwrap_or(u16::MAX),
            stripped: u16::try_from(stripped).unwrap_or(u16::MAX),
            held: u16::try_from(held).unwrap_or(u16::MAX),
            p_min,
        }))
        .await?;
        let too_many = stripped * 2 > total;
        if too_many && self.composed_by_model && !self.regenerated {
            self.regenerated = true;
            return Ok(Next::To(State::DataChecked));
        }
        self.withheld_note = too_many;
        self.kept = kept;
        if held > 0 && self.signoff.is_none() {
            self.signoff = Some(SignoffReason::ReviewBand);
        }
        Ok(Next::To(State::OutputChecked))
    }

    /// `OutputChecked → Delivered`: the kept sentences without their citations, the facts they
    /// cite, the lowest confidence among them; then `done`.
    async fn deliver(&mut self) -> Step {
        let mut cited: Vec<String> = Vec::new();
        for sentence in &self.kept {
            for id in compose::citations(sentence) {
                if !cited.contains(&id) {
                    cited.push(id);
                }
            }
        }
        let used: Vec<&Unit> = cited
            .iter()
            .filter_map(|id| self.units.iter().find(|u| &u.id == id))
            .collect();
        let mut parts: Vec<String> = self.kept.iter().map(|s| compose::without_citations(s)).collect();
        let confidence = if self.kept.is_empty() {
            if self.units.is_empty() {
                parts.push(NOTHING_FOUND.to_owned());
                1.0
            } else {
                0.0
            }
        } else {
            used.iter().map(|u| u.confidence).fold(1.0, f64::min)
        };
        // a finding the probe dropped or a check withheld is a detail left out too, when the answer
        // rests on one the reader did give
        let rests_on_a_finding = used.iter().any(|u| matches!(u.kind, facts::UnitKind::Finding));
        let lost_a_finding = self.replies.iter().any(facts::lost_a_finding);
        if self.withheld_note
            || (self.kept.is_empty() && !self.units.is_empty())
            || (rests_on_a_finding && lost_a_finding)
        {
            parts.push(compose::WITHHELD_NOTE.to_owned());
        }
        for tool in &self.missing {
            parts.push(texts::data_unread(*tool));
        }
        // an open question whose project has no reader: the answer is the facts', and says so
        if self.class == Some(QuestionClass::Open) && !self.reads_code {
            parts.push(texts::CODE_NOT_READ.to_owned());
        }
        let stale = self
            .registered
            .as_ref()
            .is_some_and(|r| r.status == ProbeStatus::Stale);
        if stale && let Some(as_of) = self.replies.iter().map(|r| facts::as_of(&r.reply)).min() {
            parts.push(texts::probe_stale(as_of));
        } else if let Some(as_of) = used.iter().filter_map(|u| u.as_of).min() {
            // figures change by the hour: the answer says when they were counted (the stale note says it already)
            parts.push(texts::figures_as_of(as_of));
        }
        let text = parts.join(" ");
        let answer = Answer {
            text: text.clone(),
            facts: used.iter().filter_map(|u| u.fact.clone()).collect(),
            confidence: ratio(confidence),
        };
        let composer = if self.composed_by_model {
            ComposerMode::Model
        } else {
            ComposerMode::Template
        };
        if let Some(reason) = self.signoff {
            // the answer it would have been waits for an approver, who delivers it unchanged
            let held = crate::approvals::Held {
                reason,
                draft: answer,
                flags: std::mem::take(&mut self.flags),
                composer,
            };
            let now = self.now();
            let recorded = crate::approvals::hold(&self.o.db, &mut self.events, held, now)
                .await
                .map_err(|err| match err {
                    crate::approvals::HoldError::Audit(err) => Stop::Audit(err),
                    crate::approvals::HoldError::Id(err) => Stop::Internal(err),
                })?;
            self.last_seq = Some(recorded.seq());
            return Ok(Next::End(Ending::PendingSignoff));
        }
        // the answer and its done are one write: an answer is in the audit only with its ending, so a
        // done that can't be written leaves no answer behind
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (cost_usd, judge_mode) = (self.cost_so_far(), self.judge_mode());
        let recorded = self
            .events
            .emit_delivered(answer, move |audit_id| Done {
                duration_ms,
                cost_usd,
                judge_mode,
                composer,
                audit_id,
            })
            .await
            .map_err(Stop::Audit)?;
        self.last_seq = Some(recorded.seq());
        // the row keeps an answer only once it is delivered
        self.answer_text = Some(text);
        Ok(Next::End(Ending::Delivered))
    }

    /// The weakest judge any stage had.
    fn judge_mode(&self) -> JudgeMode {
        self.modes.iter().copied().max().unwrap_or(JudgeMode::Rules)
    }

    /// The question failed: its `error`, if a row can still be written.
    async fn fail(&mut self, stop: Stop) -> Ending {
        tracing::warn!(error = %stop, "a question failed");
        let code = stop.code();
        let event = Event::Error(EventError {
            code,
            message: stop.message(),
            cost_usd: self.cost_so_far(),
            judge_mode: self.judged_so_far(),
        });
        if let Err(err) = self.emit(event).await {
            tracing::error!(error = %err, "the question's error can't be recorded either");
        }
        Ending::Failed(code)
    }

    /// What the emitter doesn't write into the question's row: the prompt version, the delivered
    /// answer and the question as it left the core; and the status, the judge and the cost again,
    /// for an ending whose last event couldn't be written. (Every terminal event writes its own
    /// cost and judge into the row in its transaction.)
    async fn finish(&self, ending: Ending) {
        let id = self.asked.id.as_str();
        let status = status_name(ending.status());
        let judge_mode = (!self.modes.is_empty()).then(|| mode_name(self.judge_mode()));
        let prompt_version = self.prompt_version.as_deref();
        let cost = self.cost;
        let answer = self.answer_text.as_deref();
        // redacted with the project's rules once it has one, the candidates' before
        let question = (!self.text.is_empty()).then_some(self.text.as_str());
        if ending == Ending::PendingSignoff {
            // a held answer hasn't finished, and an approver may already have decided on it: its
            // status, cost, judge and answer are the events' to write, so only what they don't hold
            let written = sqlx::query!(
                "UPDATE questions SET prompt_version = ?, text_redacted = COALESCE(?, text_redacted) WHERE id = ?",
                prompt_version,
                question,
                id
            )
            .execute(self.o.db.pool())
            .await;
            if let Err(err) = written {
                tracing::error!(error = %err, "the question's row can't be updated");
            }
            return;
        }
        let finished = db::time(self.now());
        let written = sqlx::query!(
            "UPDATE questions SET status = ?, judge_mode = ?, prompt_version = ?, cost_usd = ?, \
             answer_redacted = ?, text_redacted = COALESCE(?, text_redacted), \
             finished_at = COALESCE(finished_at, ?) WHERE id = ?",
            status,
            judge_mode,
            prompt_version,
            cost,
            answer,
            question,
            finished,
            id
        )
        .execute(self.o.db.pool())
        .await;
        if let Err(err) = written {
            tracing::error!(error = %err, "the question's row can't be updated");
        }
    }
}

impl From<questions::Asked> for Asked {
    fn from(question: questions::Asked) -> Self {
        Self {
            id: question.id,
            granted: question
                .caller
                .projects
                .iter()
                .filter_map(|grant| ProjectId::parse(&grant.project).ok())
                .collect(),
            user_id: question.caller.user_id,
            role: question.caller.role,
            text: question.text,
            project: question.project,
            clarify: question.clarify,
        }
    }
}

/// `POST /v1/questions` runs the orchestrator for each question it accepts.
#[async_trait]
impl questions::Runner for Orchestrator {
    async fn run(&self, question: questions::Asked, events: Emitter) {
        let ending = Orchestrator::run(self, question.into(), events).await;
        tracing::debug!(?ending, "a question ended");
    }
}

/// A probability as the events carry it; one that isn't a number reads as 0.
#[allow(
    clippy::expect_used,
    reason = "a finite value clamped to 0–1 is always a ratio"
)]
fn ratio(p: f64) -> Ratio {
    let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 };
    Ratio::new(p).expect("clamped to 0–1")
}

fn class_of(name: &str) -> Option<QuestionClass> {
    serde_json::from_value(Value::String(name.to_owned())).ok()
}

fn status_name(status: QuestionStatus) -> &'static str {
    match status {
        QuestionStatus::Running => "running",
        QuestionStatus::PendingSignoff => "pending_signoff",
        QuestionStatus::Delivered => "delivered",
        QuestionStatus::Refused => "refused",
        QuestionStatus::Clarify => "clarify",
        QuestionStatus::Failed => "failed",
    }
}

fn mode_name(mode: JudgeMode) -> &'static str {
    match mode {
        JudgeMode::Jev => "jev",
        JudgeMode::Chat => "chat",
        JudgeMode::Rules => "rules",
    }
}

/// The backend of the judge whose mode an outcome names, as `models.yaml` sets up the chain (see
/// `build`): Jev is the `judge` role; the chat judge is the `judge` role when that is a chat
/// judge, else `judge_fallback`. `None` for the rules, which no backend answers.
pub(crate) fn judge_backend(models: &ModelsConfig, mode: JudgeMode) -> Option<BackendName> {
    let judge = &models.roles.judge;
    match mode {
        JudgeMode::Jev => Some(judge.backend),
        JudgeMode::Chat if judge.kind == JudgeKind::Chat => Some(judge.backend),
        JudgeMode::Chat => models.roles.judge_fallback.as_ref().map(|role| role.backend),
        JudgeMode::Rules => None,
    }
}

/// The client names to look for in a sentence: every redaction pattern, global and the project's.
/// A pattern that doesn't compile ends the question: a rule left out would let its name through.
fn client_patterns(config: &Config, manifest: Option<&ProbeManifest>) -> Result<Vec<Regex>, Stop> {
    config
        .policies
        .global
        .redact
        .iter()
        .chain(manifest.map(|m| m.redact.as_slice()).unwrap_or_default())
        .map(|rule| {
            Regex::new(&rule.pattern)
                .map_err(|_| Stop::Internal("a redaction pattern doesn't compile".to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The input checks that refuse, in the order the first that doesn't pass is the one that says
    /// so: each gives its own refusal code, and each code has words of its own.
    #[test]
    fn each_input_risk_refuses_with_its_own_code_and_words() {
        let ids: Vec<&str> = RISKS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                "asks_for_code",
                "asks_to_evaluate_people",
                "asks_for_client_commitment",
                "asks_for_confidential",
                "contains_agent_instruction"
            ]
        );
        let codes: Vec<RefusalCode> = RISKS.iter().map(|(_, code)| *code).collect();
        assert_eq!(codes[3], RefusalCode::Confidential);
        for (i, code) in codes.iter().enumerate() {
            assert!(!codes[..i].contains(code), "{code:?} refuses for one check only");
            let refusal = texts::by_check(*code, &[]);
            assert_eq!(refusal.code, *code);
            assert!(
                !refusal.message.is_empty() && !refusal.hint.is_empty(),
                "{code:?}"
            );
        }
    }

    #[test]
    fn the_weakest_judge_names_the_question() {
        let mut modes = vec![JudgeMode::Jev, JudgeMode::Chat];
        assert_eq!(modes.iter().copied().max(), Some(JudgeMode::Chat));
        modes.push(JudgeMode::Rules);
        assert_eq!(modes.iter().copied().max(), Some(JudgeMode::Rules));
    }
}
