//! The events of a question's stream (`GET /v1/questions/{id}/events`). Each one is also an
//! audit row, written before the event is sent, which is why a replay after a reconnect is exact.
//!
//! On the wire an event is `event: <name>` and `data: <json>`. As one JSON value, in the audit
//! and in recordings, it is `{"event": <name>, "data": …}`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::{Accepted, Answer, ErrorCode, Refusal, RefusalCode, SignoffReason, Usd};
use crate::enums::{AccessMode, JudgeMode, QuestionClass, ToolName, Verdict};
use crate::facts::Ratio;
use crate::ids::{ApprovalId, CheckId, FieldPointer, ProjectId};
use crate::tools::{
    AskCodeArgs, BlockedArgs, ChangesArgs, DescribeArgs, HealthArgs, ProgressArgs, TestedArgs,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    Accepted(Accepted),
    Project(ProjectPicked),
    Classified(Classified),
    Refused(Refused),
    Clarify(Clarify),
    Plan(Plan),
    ToolChecked(ToolChecked),
    ToolResult(ToolResult),
    Composing(Composing),
    Judged(Judged),
    PendingSignoff(PendingSignoff),
    Answer(Answer),
    Done(Done),
    Error(EventError),
}

impl Event {
    /// Every event's name, in the order a question meets them.
    pub const NAMES: [&'static str; 14] = [
        "accepted",
        "project",
        "classified",
        "refused",
        "clarify",
        "plan",
        "tool_checked",
        "tool_result",
        "composing",
        "judged",
        "pending_signoff",
        "answer",
        "done",
        "error",
    ];

    /// The name the stream sends as `event:`.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Accepted(_) => "accepted",
            Event::Project(_) => "project",
            Event::Classified(_) => "classified",
            Event::Refused(_) => "refused",
            Event::Clarify(_) => "clarify",
            Event::Plan(_) => "plan",
            Event::ToolChecked(_) => "tool_checked",
            Event::ToolResult(_) => "tool_result",
            Event::Composing(_) => "composing",
            Event::Judged(_) => "judged",
            Event::PendingSignoff(_) => "pending_signoff",
            Event::Answer(_) => "answer",
            Event::Done(_) => "done",
            Event::Error(_) => "error",
        }
    }

    /// Whether the stream ends after this event. A pending sign-off ends it until an approver
    /// decides; the decision continues the question with `answer` and `done`, or `refused`.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Event::Refused(_)
                | Event::Clarify(_)
                | Event::PendingSignoff(_)
                | Event::Done(_)
                | Event::Error(_)
        )
    }
}

/// The project the question is about, and who chose it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ProjectPicked {
    pub project: ProjectId,
    pub p: Ratio,
    pub source: ProjectSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSource {
    /// The person named it.
    Explicit,
    /// The judge picked it from the person's projects.
    Judge,
}

/// The question's class and the input checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Classified {
    pub class: QuestionClass,
    pub p: Ratio,
    pub checks: Vec<CheckResult>,
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct CheckResult {
    pub id: CheckId,
    pub source: CheckSource,
    /// The judge's probability; left out when a rule decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<Ratio>,
    pub verdict: Verdict,
}

/// What decided a check: a rule, or the judge that answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    Rule,
    Jev,
    /// The chat fallback judge.
    Chat,
}

/// The question was refused: why, what the person can do instead, and what it cost until then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Refused {
    pub code: RefusalCode,
    pub message: String,
    pub hint: String,
    /// What the question cost until it was refused.
    pub cost_usd: Usd,
    /// The weakest judge any stage had; `null` when no judge answered before the refusal.
    #[schema(required = true)]
    pub judge_mode: Option<JudgeMode>,
}

impl Refused {
    /// The refusal with what the question cost until then.
    pub fn new(refusal: Refusal, cost_usd: Usd, judge_mode: Option<JudgeMode>) -> Self {
        let Refusal { code, message, hint } = refusal;
        Self {
            code,
            message,
            hint,
            cost_usd,
            judge_mode,
        }
    }

    /// The refusal alone, as `GET /v1/questions/{id}` shows it.
    pub fn refusal(&self) -> Refusal {
        Refusal {
            code: self.code,
            message: self.message.clone(),
            hint: self.hint.clone(),
        }
    }
}

/// The question can't go on without the person's choice; it ends here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Clarify {
    pub kind: ClarifyKind,
    pub question: String,
    pub options: Vec<ClarifyOption>,
    /// What the question cost until it asked back.
    pub cost_usd: Usd,
    /// The weakest judge any stage had; `null` when no judge answered before it.
    #[schema(required = true)]
    pub judge_mode: Option<JudgeMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClarifyKind {
    Project,
    Class,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ClarifyOption {
    /// The project id or the class; sent back as `clarify.option`.
    pub id: String,
    pub label: String,
}

/// The tool calls the fixed class table chose, with the arguments the planner filled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Plan {
    pub calls: Vec<ToolCall>,
}

/// One probe tool call: `{"tool": <name>, "args": {…}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(tag = "tool", content = "args", rename_all = "snake_case")]
pub enum ToolCall {
    Describe(DescribeArgs),
    Progress(ProgressArgs),
    Tested(TestedArgs),
    Blocked(BlockedArgs),
    Health(HealthArgs),
    Changes(ChangesArgs),
    AskCode(AskCodeArgs),
}

impl ToolCall {
    pub fn tool(&self) -> ToolName {
        match self {
            ToolCall::Describe(_) => ToolName::Describe,
            ToolCall::Progress(_) => ToolName::Progress,
            ToolCall::Tested(_) => ToolName::Tested,
            ToolCall::Blocked(_) => ToolName::Blocked,
            ToolCall::Health(_) => ToolName::Health,
            ToolCall::Changes(_) => ToolName::Changes,
            ToolCall::AskCode(_) => ToolName::AskCode,
        }
    }
}

/// The checks on one call, before it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ToolChecked {
    /// The call's index in `plan.calls`.
    pub call: u16,
    pub checks: Vec<CheckResult>,
}

/// What one call returned, counted, never shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ToolResult {
    /// The call's index in `plan.calls`.
    pub call: u16,
    pub items: u32,
    /// Text values a data check withheld, as JSON Pointers into the reply.
    pub withheld_fields: Vec<FieldPointer>,
    /// The probe's effective mode when it answered.
    pub access: AccessMode,
    /// The lowest probability that the reply is fine, over the call's data checks a judge
    /// answered (`field_contains_agent_instruction` on each text of the reply, findings
    /// included): a check's `p` when its polarity in `checks.yaml` is `legit`, `1 - p` when it is
    /// `risk`. Left out, like a check's `p`, when only rules decided or there was nothing to check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_min: Option<Ratio>,
}

/// The answer is being written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Composing {}

/// The output checks: how many sentences, how many were stripped, how many held for sign-off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Judged {
    pub sentences: u16,
    pub stripped: u16,
    pub held: u16,
    /// The lowest probability that the answer is fine, over the checks a judge answered on the
    /// sentences the person reads, delivered or held: a check's `p` when its polarity in
    /// `checks.yaml` is `legit`, `1 - p` when it is `risk`. Left out, like a check's `p`, when only
    /// rules decided or there was nothing to check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_min: Option<Ratio>,
}

/// The answer waits for an approver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct PendingSignoff {
    pub approval_id: ApprovalId,
    pub reason: SignoffReason,
}

/// The question is finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Done {
    pub duration_ms: u64,
    pub cost_usd: Usd,
    /// The weakest judge any stage had.
    pub judge_mode: JudgeMode,
    /// Who wrote the answer.
    pub composer: ComposerMode,
    pub audit_id: u64,
}

/// Who wrote an answer: the composer model, or the template composer when the model couldn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComposerMode {
    Model,
    Template,
}

/// The question failed after it started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct EventError {
    pub code: ErrorCode,
    pub message: String,
    /// What the question cost until it failed.
    pub cost_usd: Usd,
    /// The weakest judge any stage had; `null` when no judge answered before the failure.
    #[schema(required = true)]
    pub judge_mode: Option<JudgeMode>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn an_event_is_its_name_and_its_data() {
        let event: Event = serde_json::from_value(json!({
            "event": "classified",
            "data": { "class": "progress", "p": 0.94,
                      "checks": [{ "id": "asks_for_code", "source": "jev", "p": 0.02, "verdict": "pass" }] }
        }))
        .unwrap();
        assert_eq!(event.name(), "classified");
        assert!(!event.is_terminal());
        let Event::Classified(classified) = &event else {
            panic!("not classified: {event:?}");
        };
        assert_eq!(classified.checks[0].source, CheckSource::Jev);
    }

    #[test]
    fn names_match_the_wire() {
        let composing = Event::Composing(Composing {});
        assert_eq!(
            serde_json::to_value(&composing).unwrap(),
            json!({ "event": "composing", "data": {} })
        );
        assert!(Event::NAMES.contains(&composing.name()));
        assert_eq!(Event::NAMES.len(), 14);
    }

    #[test]
    fn a_rule_check_has_no_probability() {
        let check = CheckResult {
            id: CheckId::parse("arguments_valid").unwrap(),
            source: CheckSource::Rule,
            p: None,
            verdict: Verdict::Pass,
        };
        assert_eq!(
            serde_json::to_value(&check).unwrap(),
            json!({ "id": "arguments_valid", "source": "rule", "verdict": "pass" })
        );
    }

    #[test]
    fn a_tool_call_is_the_tool_and_its_arguments() {
        let call: ToolCall = serde_json::from_value(
            json!({ "tool": "progress", "args": { "since": "2026-09-28", "limit": 20 } }),
        )
        .unwrap();
        assert_eq!(call.tool(), ToolName::Progress);
        assert!(
            serde_json::from_value::<ToolCall>(json!({ "tool": "progress", "args": { "path": "src" } }))
                .is_err()
        );
        assert!(serde_json::from_value::<ToolCall>(json!({ "tool": "read_file", "args": {} })).is_err());
    }

    #[test]
    fn a_stage_without_a_judge_has_no_lowest_probability() {
        let result = ToolResult {
            call: 0,
            items: 3,
            withheld_fields: vec![],
            access: AccessMode::Code,
            p_min: None,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "call": 0, "items": 3, "withheld_fields": [], "access": "code" })
        );
        let judged = Judged {
            sentences: 2,
            stripped: 0,
            held: 0,
            p_min: None,
        };
        assert_eq!(
            serde_json::to_value(&judged).unwrap(),
            json!({ "sentences": 2, "stripped": 0, "held": 0 })
        );
        // a recording made before the field existed still reads
        let old: Event = serde_json::from_value(json!({ "event": "tool_result", "data": {
            "call": 0, "items": 2, "withheld_fields": ["/items/1/title"], "access": "code" } }))
        .unwrap();
        let Event::ToolResult(old) = old else {
            panic!("not a tool result: {old:?}");
        };
        assert_eq!(old.p_min, None);
        let old: Event = serde_json::from_value(json!({ "event": "judged", "data": {
            "sentences": 2, "stripped": 1, "held": 0 } }))
        .unwrap();
        let Event::Judged(old) = old else {
            panic!("not judged: {old:?}");
        };
        assert_eq!(old.p_min, None);
    }

    #[test]
    fn the_lowest_probability_is_a_ratio() {
        let judged: Judged =
            serde_json::from_value(json!({ "sentences": 3, "stripped": 0, "held": 1, "p_min": 0.87 }))
                .unwrap();
        assert_eq!(judged.p_min.map(Ratio::get), Some(0.87));
        assert_eq!(serde_json::to_value(&judged).unwrap()["p_min"], json!(0.87));
        assert!(
            serde_json::from_value::<Judged>(
                json!({ "sentences": 3, "stripped": 0, "held": 1, "p_min": 1.2 })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ToolResult>(json!({
                "call": 1, "items": 2, "withheld_fields": [], "access": "code_model", "p_min": 1.2 }))
            .is_err()
        );
    }

    #[test]
    fn an_ask_code_call_carries_the_redacted_question() {
        let call: ToolCall = serde_json::from_value(json!({
            "tool": "ask_code", "args": { "question": "How does the export pick the file format?" } }))
        .unwrap();
        assert_eq!(call.tool(), ToolName::AskCode);
        let ToolCall::AskCode(args) = &call else {
            panic!("not ask_code: {call:?}");
        };
        assert_eq!(
            args.question.as_str(),
            "How does the export pick the file format?"
        );
        // a plan in a log never carries the question
        assert!(!format!("{call:?}").contains("export"), "{call:?}");
        assert!(
            serde_json::from_value::<ToolCall>(json!({
                "tool": "ask_code", "args": { "question": "Why?", "module": "Export" } }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ToolCall>(json!({
                "tool": "ask_code", "args": { "question": "line one\nline two" } }))
            .is_err()
        );
    }

    #[test]
    fn the_terminal_events() {
        let terminal: Vec<&str> = [
            Event::Refused(Refused::new(
                Refusal {
                    code: RefusalCode::AsksForCode,
                    message: String::new(),
                    hint: String::new(),
                },
                Usd::default(),
                None,
            )),
            Event::Composing(Composing {}),
            Event::Error(EventError {
                code: ErrorCode::Internal,
                message: String::new(),
                cost_usd: Usd::default(),
                judge_mode: None,
            }),
        ]
        .iter()
        .filter(|e| e.is_terminal())
        .map(Event::name)
        .collect();
        assert_eq!(terminal, ["refused", "error"]);
    }

    #[test]
    fn every_ending_says_what_it_cost_and_which_judge_answered() {
        let refused: Event = serde_json::from_value(json!({ "event": "refused", "data": {
            "code": "asks_for_code", "message": "No code.", "hint": "Ask what is done.",
            "cost_usd": 0.002, "judge_mode": "chat" } }))
        .unwrap();
        let Event::Refused(refused) = refused else {
            panic!("not refused: {refused:?}");
        };
        assert_eq!(refused.judge_mode, Some(JudgeMode::Chat));
        assert_eq!(
            serde_json::to_value(refused.refusal()).unwrap(),
            json!({ "code": "asks_for_code", "message": "No code.", "hint": "Ask what is done." })
        );
        // before any judge answered, the mode is null, and still written out
        let error = Event::Error(EventError {
            code: ErrorCode::ProbeUnavailable,
            message: "The probe didn't answer.".to_owned(),
            cost_usd: Usd::new(0.0).unwrap(),
            judge_mode: None,
        });
        assert_eq!(
            serde_json::to_value(&error).unwrap()["data"]["judge_mode"],
            Value::Null
        );
        // an ending without its cost doesn't read
        assert!(
            serde_json::from_value::<Event>(json!({ "event": "error", "data": {
                "code": "internal", "message": "Failed.", "judge_mode": null } }))
            .is_err()
        );
    }

    #[test]
    fn done_says_who_wrote_the_answer() {
        let done: Event = serde_json::from_value(json!({ "event": "done", "data": {
            "duration_ms": 1900, "cost_usd": 0.004, "judge_mode": "rules", "composer": "template",
            "audit_id": 1834 } }))
        .unwrap();
        let Event::Done(done) = done else {
            panic!("not done: {done:?}");
        };
        assert_eq!(done.composer, ComposerMode::Template);
        assert!(
            serde_json::from_value::<Event>(json!({ "event": "done", "data": {
                "duration_ms": 1900, "cost_usd": 0.004, "judge_mode": "jev", "audit_id": 1834 } }))
            .is_err()
        );
    }
}
