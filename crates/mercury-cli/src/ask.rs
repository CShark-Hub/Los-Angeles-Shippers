//! `mercury ask`: one loop over a question's events. Each event goes to the summary, which
//! becomes the `--json` object and the exit code, and to the view, the live timeline, so the two
//! can't disagree. A broken stream is resumed with `Last-Event-ID`; `--wait` keeps resuming while
//! the answer waits for an approver. Asking has an overall deadline, so a script never hangs on a
//! question that stopped streaming; `--wait` has none.

use std::future::Future;
use std::time::Duration;

use mercury_schema::api::{Answer, ClarifyAnswer, ErrorCode, NewQuestion, QuestionStatus, Usd};
use mercury_schema::events::{Clarify, ComposerMode, Event, EventError, PendingSignoff, Refused, ToolCall};
use mercury_schema::ids::{ProjectId, QuestionId};
use mercury_schema::{JudgeMode, QuestionClass};
use serde::Serialize;
use tokio::time::Instant;

use crate::client::{Client, IdempotencyKey, StreamError, event_of};
use crate::error::CliError;
use crate::render::View;

/// How many times in a row a broken stream is resumed before the CLI gives up on it.
pub const MAX_RESUMES: u32 = 5;
/// The pause before the first resume; it doubles with each one.
pub const FIRST_PAUSE: Duration = Duration::from_millis(500);
/// The longest pause of `--wait` between two looks at a question waiting for sign-off.
pub const WAIT_PAUSE_MAX: Duration = Duration::from_secs(30);
/// How long asking waits for the question's end unless `MERCURY_ASK_DEADLINE` says otherwise.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(120);
/// The longest deadline `MERCURY_ASK_DEADLINE` may name: a day.
pub const MAX_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// How a question ended. The exit codes are a contract: scripts and agents rely on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Answered,
    Refused,
    Clarify,
    PendingSignoff,
    /// An `error` event, an end without an answer, or a stream lost for good.
    Failed,
}

impl Outcome {
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Answered => 0,
            Outcome::Failed => 1,
            Outcome::Refused => 2,
            Outcome::Clarify => 3,
            Outcome::PendingSignoff => 4,
        }
    }
}

/// What the events told about a question; `--json` prints it. The fields of
/// `GET /v1/questions/{id}`, plus what a script needs to go on: the clarify's options, the
/// sign-off's approval, the probe calls, the error and who wrote the answer. Every field is
/// present, `null` when the events didn't say: `cost_usd` and `judge_mode` come with every ending
/// (`judge_mode` stays `null` when no judge answered), `composer`, `duration_ms` and `audit_id`
/// with `done`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub id: QuestionId,
    /// `running` when the stream was lost before the end.
    pub status: QuestionStatus,
    pub project: Option<ProjectId>,
    pub class: Option<QuestionClass>,
    pub answer: Option<Answer>,
    pub refusal: Option<Refused>,
    pub clarify: Option<Clarify>,
    pub signoff: Option<PendingSignoff>,
    /// The probe calls the class table chose, with the arguments the planner filled.
    pub plan: Option<Vec<ToolCall>>,
    pub error: Option<EventError>,
    pub judge_mode: Option<JudgeMode>,
    pub cost_usd: Option<Usd>,
    /// Who wrote the answer: the composer model, or the template while it can't.
    pub composer: Option<ComposerMode>,
    pub duration_ms: Option<u64>,
    pub audit_id: Option<u64>,
}

impl Summary {
    pub fn new(id: QuestionId) -> Self {
        Self {
            id,
            status: QuestionStatus::Running,
            project: None,
            class: None,
            answer: None,
            refusal: None,
            clarify: None,
            signoff: None,
            plan: None,
            error: None,
            judge_mode: None,
            cost_usd: None,
            composer: None,
            duration_ms: None,
            audit_id: None,
        }
    }

    /// What an ending says the question cost, and which judge answered (none, when none did).
    fn ended_at(&mut self, cost: Usd, judge: Option<JudgeMode>) {
        self.cost_usd = Some(cost);
        self.judge_mode = judge;
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Project(picked) => self.project = Some(picked.project.clone()),
            Event::Classified(classified) => self.class = Some(classified.class),
            Event::Refused(refusal) => {
                self.status = QuestionStatus::Refused;
                self.refusal = Some(refusal.clone());
                self.ended_at(refusal.cost_usd, refusal.judge_mode);
            }
            Event::Clarify(clarify) => {
                self.status = QuestionStatus::Clarify;
                self.clarify = Some(clarify.clone());
                self.ended_at(clarify.cost_usd, clarify.judge_mode);
            }
            Event::Plan(plan) => self.plan = Some(plan.calls.clone()),
            Event::PendingSignoff(signoff) => {
                self.status = QuestionStatus::PendingSignoff;
                self.signoff = Some(signoff.clone());
            }
            Event::Answer(answer) => self.answer = Some(answer.clone()),
            Event::Done(done) => {
                // an end without an answer is no answer
                self.status = if self.answer.is_some() {
                    QuestionStatus::Delivered
                } else {
                    QuestionStatus::Failed
                };
                self.ended_at(done.cost_usd, Some(done.judge_mode));
                self.composer = Some(done.composer);
                self.duration_ms = Some(done.duration_ms);
                self.audit_id = Some(done.audit_id);
            }
            Event::Error(error) => {
                self.status = QuestionStatus::Failed;
                self.error = Some(error.clone());
                self.ended_at(error.cost_usd, error.judge_mode);
            }
            Event::Accepted(_)
            | Event::ToolChecked(_)
            | Event::ToolResult(_)
            | Event::Composing(_)
            | Event::Judged(_) => {}
        }
    }

    pub fn outcome(&self) -> Outcome {
        match self.status {
            QuestionStatus::Delivered => Outcome::Answered,
            QuestionStatus::Refused => Outcome::Refused,
            QuestionStatus::Clarify => Outcome::Clarify,
            QuestionStatus::PendingSignoff => Outcome::PendingSignoff,
            QuestionStatus::Running | QuestionStatus::Failed => Outcome::Failed,
        }
    }
}

/// A question to ask.
#[derive(Debug, Clone)]
pub struct Question {
    pub text: String,
    pub project: Option<ProjectId>,
    pub clarify: Option<ClarifyAnswer>,
    /// The caller's own question an approver rejected, which this one asks again.
    pub follows: Option<QuestionId>,
}

/// A followed question: what its events told, and whether the deadline ended the wait.
#[derive(Debug)]
pub struct Followed {
    pub summary: Summary,
    /// The deadline that passed before the question ended, if one did.
    pub deadline_passed: Option<Duration>,
}

impl Followed {
    fn ended(summary: Summary) -> Self {
        Self {
            summary,
            deadline_passed: None,
        }
    }
}

/// When the wait for a question's end stops: the instant, and the limit it was counted from.
#[derive(Debug, Clone, Copy)]
struct Deadline {
    at: Instant,
    limit: Duration,
}

/// Runs `future` until the deadline, if there is one; `None` once the deadline has passed.
async fn before<T>(deadline: Option<Deadline>, future: impl Future<Output = T>) -> Option<T> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline.at, future).await.ok(),
        None => Some(future.await),
    }
}

/// Asks, then follows the question to its end: an answer, a refusal, a clarify, a pending
/// sign-off or an error. Once `deadline` has passed since the call began, it stops waiting and
/// gives the summary as it stands, `running`, with the deadline in the result.
pub async fn ask(
    client: &Client,
    question: Question,
    deadline: Duration,
    view: &mut dyn View,
) -> Result<Followed, CliError> {
    let deadline = Deadline {
        at: Instant::now() + deadline,
        limit: deadline,
    };
    let body = NewQuestion {
        text: question.text,
        project: question.project,
        clarify: question.clarify,
        follows: question.follows,
    };
    let key = IdempotencyKey::new()?;
    let accepted = match client.ask(&body, &key).await {
        // the same key again: if the first request arrived, the server knows the question
        Err(CliError::Unreachable { .. }) => client.ask(&body, &key).await?,
        other => other?,
    };
    follow(client, accepted.id, Until::End, Some(deadline), view).await
}

/// Follows a question asked before through its sign-off, if it has one, to its final state. There
/// is no deadline: an approver may take as long as the approval lasts.
pub async fn wait(client: &Client, id: QuestionId, view: &mut dyn View) -> Result<Followed, CliError> {
    follow(client, id, Until::Decided, None, view).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Until {
    /// The first terminal event, a pending sign-off included.
    End,
    /// Past a pending sign-off, to the approver's decision.
    Decided,
}

/// The event loop. A stream that breaks or closes before the end is resumed after the last event
/// seen, up to `MAX_RESUMES` times in a row; then the summary is returned as it stands, `running`.
/// Before it is resumed, the question's state is asked: the core marks a question `failed` without
/// an event when it can't write the event that would end it, and no resume would ever bring one.
/// So is it when the deadline passes, whatever the stream is doing: waiting for headers, kept open
/// by keep-alives that carry no event, or between two resumes.
async fn follow(
    client: &Client,
    id: QuestionId,
    until: Until,
    deadline: Option<Deadline>,
    view: &mut dyn View,
) -> Result<Followed, CliError> {
    let mut summary = Summary::new(id.clone());
    let mut last_id: Option<String> = None;
    let mut resumes = 0;
    let mut wait_pause = FIRST_PAUSE;
    let passed = |summary: Summary| {
        Ok(Followed {
            summary,
            deadline_passed: deadline.map(|d| d.limit),
        })
    };
    loop {
        let Some(opened) = before(deadline, client.events(&id, last_id.as_deref())).await else {
            return passed(summary);
        };
        let streamed = opened.is_ok();
        match opened {
            Ok(mut stream) => loop {
                let Some(next) = before(deadline, stream.next_frame()).await else {
                    return passed(summary);
                };
                match next {
                    Ok(Some(frame)) => {
                        if frame.last_id.is_some() {
                            last_id.clone_from(&frame.last_id);
                        }
                        let Some(event) = event_of(&frame)? else {
                            continue;
                        };
                        resumes = 0;
                        summary.apply(&event);
                        view.event(&event)?;
                        if event.is_terminal() {
                            if until == Until::Decided && matches!(event, Event::PendingSignoff(_)) {
                                break;
                            }
                            return Ok(Followed::ended(summary));
                        }
                    }
                    Ok(None) | Err(StreamError::Silent | StreamError::Dropped) => break,
                    Err(StreamError::Malformed(err)) => return Err(CliError::Protocol(err.to_string())),
                }
            },
            Err(err) if err.is_transient() => {}
            Err(err) => return Err(err),
        }
        if streamed {
            // the stream ended without a terminal event
            match before(deadline, client.question(&id)).await {
                None => return passed(summary),
                Some(Ok(state)) if state.status == QuestionStatus::Failed => {
                    summary.status = QuestionStatus::Failed;
                    summary.error = Some(EventError {
                        code: ErrorCode::Internal,
                        message: "The server could not record how the question ended.".to_owned(),
                        cost_usd: state.cost_usd,
                        judge_mode: state.judge_mode,
                    });
                    return Ok(Followed::ended(summary));
                }
                // still going, or the server can't say: resume as before
                Some(_) => {}
            }
        }
        if until == Until::Decided && summary.status == QuestionStatus::PendingSignoff {
            if before(deadline, tokio::time::sleep(wait_pause)).await.is_none() {
                return passed(summary);
            }
            wait_pause = (wait_pause * 2).min(WAIT_PAUSE_MAX);
            continue;
        }
        resumes += 1;
        if resumes > MAX_RESUMES {
            return Ok(Followed::ended(summary));
        }
        if before(deadline, tokio::time::sleep(FIRST_PAUSE * 2u32.pow(resumes - 1)))
            .await
            .is_none()
        {
            return passed(summary);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(value: serde_json::Value) -> Event {
        serde_json::from_value(value).unwrap()
    }

    fn id() -> QuestionId {
        QuestionId::parse("q_01J9Z3QK7M8N9P0R1S2T3V4W5X").unwrap()
    }

    fn answered() -> Vec<Event> {
        vec![
            event(json!({ "event": "accepted", "data": { "id": id() } })),
            event(
                json!({ "event": "project", "data": { "project": "delta", "p": 1.0, "source": "explicit" } }),
            ),
            event(json!({ "event": "classified", "data": { "class": "progress", "p": 0.94, "checks": [] } })),
            event(
                json!({ "event": "plan", "data": { "calls": [{ "tool": "progress", "args": { "since": "2026-09-28" } }] } }),
            ),
            event(
                json!({ "event": "answer", "data": { "text": "Two things were merged.", "facts": [], "confidence": 0.86 } }),
            ),
            event(
                json!({ "event": "done", "data": { "duration_ms": 6420, "cost_usd": 0.011, "judge_mode": "jev", "composer": "model", "audit_id": 1834 } }),
            ),
        ]
    }

    fn summary_of(events: &[Event]) -> Summary {
        let mut summary = Summary::new(id());
        for event in events {
            summary.apply(event);
        }
        summary
    }

    /// The core writes a refusal and a clarifying question in words of its own; the CLI prints them
    /// as they came.
    #[test]
    fn a_refusal_and_a_clarify_are_printed_as_the_core_sent_them() {
        let refused = summary_of(&[event(json!({ "event": "refused", "data": {
            "code": "asks_for_code",
            "message": "Mercury doesn't show source code, file names or paths.",
            "hint": "Ask what is done, tested or blocked in Export or Invoicing instead.",
            "cost_usd": 0.001, "judge_mode": "jev" } }))]);
        let mut out = Vec::new();
        crate::render::result(&refused, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Refused: Mercury doesn't show source code, file names or paths.\n\
             Ask what is done, tested or blocked in Export or Invoicing instead.\n"
        );

        let clarify = summary_of(&[event(json!({ "event": "clarify", "data": {
            "kind": "class", "question": "What would you like to know about delta?",
            "options": [{ "id": "blocked", "label": "What is blocked, and on what" }],
            "cost_usd": 0.0009, "judge_mode": "chat" } }))]);
        let mut out = Vec::new();
        crate::render::result(&clarify, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(
                "What would you like to know about delta?\n  blocked  What is blocked, and on what\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn an_answer_with_its_done_is_answered() {
        let summary = summary_of(&answered());
        assert_eq!(summary.status, QuestionStatus::Delivered);
        assert_eq!(summary.outcome().exit_code(), 0);
        assert_eq!(summary.class, Some(QuestionClass::Progress));
        assert_eq!(summary.plan.as_ref().map(Vec::len), Some(1));
        assert_eq!(summary.audit_id, Some(1834));
    }

    #[test]
    fn each_ending_has_its_exit_code() {
        let refused = event(json!({ "event": "refused", "data": {
            "code": "asks_for_code", "message": "Mercury doesn't show code.", "hint": "Ask what the module does.",
            "cost_usd": 0.001, "judge_mode": "jev" } }));
        // a refusal exits 2 whatever its code, a confidential one too
        let confidential = event(json!({ "event": "refused", "data": {
            "code": "confidential", "message": "Mercury doesn't share confidential information.",
            "hint": "Ask what is done in the project.", "cost_usd": 0.001, "judge_mode": "jev" } }));
        let clarify = event(json!({ "event": "clarify", "data": {
            "kind": "project", "question": "Which project?", "options": [{ "id": "delta", "label": "Delta" }],
            "cost_usd": 0.0, "judge_mode": null } }));
        let pending = event(json!({ "event": "pending_signoff", "data": {
            "approval_id": "ap_01J9Z3QK7M8N9P0R1S2T3V4W5X", "reason": "review_band" } }));
        let error = event(
            json!({ "event": "error", "data": { "code": "probe_unavailable", "message": "The probe can't answer.",
                "cost_usd": 0.002, "judge_mode": "jev" } }),
        );
        let done = event(
            json!({ "event": "done", "data": { "duration_ms": 1, "cost_usd": 0.0, "judge_mode": "rules", "composer": "template", "audit_id": 1 } }),
        );
        let codes: Vec<u8> = [
            vec![refused],
            vec![confidential],
            vec![clarify],
            vec![pending],
            vec![error],
            vec![done],
            vec![],
        ]
        .iter()
        .map(|events| summary_of(events).outcome().exit_code())
        .collect();
        assert_eq!(codes, [2, 2, 3, 4, 1, 1, 1]);
    }

    /// Every ending gives the summary what the question cost and which judge answered; `done` also
    /// who wrote the answer.
    #[test]
    fn every_ending_tells_the_summary_its_cost_and_judge_and_done_its_composer() {
        let refused = summary_of(&[event(json!({ "event": "refused", "data": {
            "code": "forbidden_project", "message": "Not yours.", "hint": "Ask about delta.",
            "cost_usd": 0.0, "judge_mode": null } }))]);
        assert_eq!(
            (refused.cost_usd, refused.judge_mode, refused.composer),
            (Some(Usd::default()), None, None)
        );
        let failed = summary_of(&[event(json!({ "event": "error", "data": {
            "code": "budget_exceeded", "message": "Over.", "cost_usd": 0.051, "judge_mode": "jev" } }))]);
        assert_eq!(
            (failed.cost_usd, failed.judge_mode),
            (Usd::new(0.051), Some(JudgeMode::Jev))
        );
        let clarify = summary_of(&[event(json!({ "event": "clarify", "data": {
            "kind": "class", "question": "Which kind?", "options": [], "cost_usd": 0.0009, "judge_mode": "chat" } }))]);
        assert_eq!(clarify.judge_mode, Some(JudgeMode::Chat));
        let template = summary_of(&[
            event(
                json!({ "event": "answer", "data": { "text": "One item is done.", "facts": [], "confidence": 1.0 } }),
            ),
            event(
                json!({ "event": "done", "data": { "duration_ms": 180, "cost_usd": 0.0,
                "judge_mode": "rules", "composer": "template", "audit_id": 9 } }),
            ),
        ]);
        assert_eq!(template.composer, Some(ComposerMode::Template));
        let json = serde_json::to_value(&template).unwrap();
        assert_eq!(
            (&json["composer"], &json["judge_mode"]),
            (&json!("template"), &json!("rules"))
        );
    }

    #[test]
    fn a_decision_after_the_sign_off_settles_it() {
        let pending = event(json!({ "event": "pending_signoff", "data": {
            "approval_id": "ap_01J9Z3QK7M8N9P0R1S2T3V4W5X", "reason": "class" } }));
        let mut events = vec![pending];
        events.extend(answered().into_iter().skip(4));
        let summary = summary_of(&events);
        assert_eq!(summary.outcome(), Outcome::Answered);
        assert!(summary.signoff.is_some(), "the sign-off stays on the record");
    }

    #[test]
    fn the_json_object_has_every_field_null_when_unknown() {
        let value = serde_json::to_value(Summary::new(id())).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = [
            "id",
            "status",
            "project",
            "class",
            "answer",
            "refusal",
            "clarify",
            "signoff",
            "plan",
            "error",
            "judge_mode",
            "cost_usd",
            "composer",
            "duration_ms",
            "audit_id",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(object["status"], "running");
        assert!(
            object
                .iter()
                .filter(|(key, _)| !["id", "status"].contains(&key.as_str()))
                .all(|(_, value)| value.is_null())
        );
    }
}
