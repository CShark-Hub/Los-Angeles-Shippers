//! What the person sees: a question's live timeline on standard error, and its result on
//! standard output: the answer with its facts, a refusal with its hint, a clarify's options or a
//! pending sign-off. With `--json`, standard output gets one JSON object instead. Text from the
//! server is shown without control characters, so it can't move the cursor or recolour the
//! terminal; JSON escapes them anyway.

use std::io::{self, Write};

use chrono::{DateTime, Utc};
#[cfg(test)]
use mercury_schema::QuestionId;
use mercury_schema::api::{
    Answer, ApprovalList, AuditItem, Decision, Me, ProjectList, QuestionStatus, RefusalCode, SignoffReason,
    Usd,
};
use mercury_schema::events::{CheckResult, Event, ProjectSource, ToolCall};
use mercury_schema::facts::Ratio;
use mercury_schema::{Fact, JudgeMode, ToolName};
use serde::Serialize;
use serde_json::Value;

use crate::ask::Summary;
use crate::credentials::Server;

/// Where the event loop shows each event as it arrives.
pub trait View {
    fn event(&mut self, event: &Event) -> io::Result<()>;
}

/// No timeline.
pub struct Quiet;

impl View for Quiet {
    fn event(&mut self, _: &Event) -> io::Result<()> {
        Ok(())
    }
}

/// One line per event: project, class, checks, probe calls, judged, answer, cost and time.
pub struct Timeline<W: Write> {
    out: W,
    tools: Vec<ToolName>,
}

impl<W: Write> Timeline<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            tools: Vec::new(),
        }
    }

    fn line(&mut self, label: &str, text: &str) -> io::Result<()> {
        writeln!(self.out, "{label:<10} {}", one_line(text))
    }

    fn tool(&self, call: u16) -> &'static str {
        self.tools
            .get(usize::from(call))
            .map_or("a call", |tool| tool.as_str())
    }
}

impl<W: Write> View for Timeline<W> {
    fn event(&mut self, event: &Event) -> io::Result<()> {
        match event {
            Event::Accepted(accepted) => self.line("asked", accepted.id.as_str()),
            Event::Project(picked) => {
                let how = match picked.source {
                    ProjectSource::Explicit => "as asked".to_owned(),
                    ProjectSource::Judge => format!("picked by the judge, p {}", p(picked.p)),
                };
                self.line("project", &format!("{} ({how})", picked.project))
            }
            Event::Classified(classified) => {
                self.line(
                    "class",
                    &format!("{} (p {})", name(&classified.class), p(classified.p)),
                )?;
                for check in &classified.checks {
                    self.line("check", &check_text(check))?;
                }
                Ok(())
            }
            Event::Refused(refusal) => self.line(
                "refused",
                &format!(
                    "{} · {}",
                    name(&refusal.code),
                    spent(refusal.cost_usd, refusal.judge_mode)
                ),
            ),
            Event::Clarify(clarify) => {
                let options = count(clarify.options.len(), "option");
                self.line(
                    "clarify",
                    &format!(
                        "{} ({options}) · {}",
                        clarify.question,
                        spent(clarify.cost_usd, clarify.judge_mode)
                    ),
                )
            }
            Event::Plan(plan) => {
                self.tools = plan.calls.iter().map(ToolCall::tool).collect();
                if plan.calls.is_empty() {
                    return self.line("plan", "no probe calls");
                }
                let calls: Vec<String> = plan.calls.iter().map(call_text).collect();
                self.line("plan", &calls.join(" · "))
            }
            Event::ToolChecked(checked) => {
                let checks: Vec<String> = checked.checks.iter().map(check_text).collect();
                let text = format!("{}: {}", self.tool(checked.call), checks.join(" · "));
                self.line(&format!("call {}", u32::from(checked.call) + 1), &text)
            }
            Event::ToolResult(result) => {
                let mut text = format!(
                    "{}: {}, access {}",
                    self.tool(result.call),
                    count(result.items as usize, "item"),
                    name(&result.access)
                );
                if !result.withheld_fields.is_empty() {
                    let withheld = count(result.withheld_fields.len(), "value");
                    text.push_str(&format!(", {withheld} withheld"));
                }
                text.push_str(&lowest(result.p_min));
                self.line(&format!("result {}", u32::from(result.call) + 1), &text)
            }
            Event::Composing(_) => self.line("composing", "writing the answer from the facts"),
            Event::Judged(judged) => self.line(
                "judged",
                &format!(
                    "{}, {} stripped, {} held for sign-off{}",
                    count(usize::from(judged.sentences), "sentence"),
                    judged.stripped,
                    judged.held,
                    lowest(judged.p_min)
                ),
            ),
            Event::PendingSignoff(signoff) => self.line(
                "sign-off",
                &format!(
                    "waiting for an approver: {} ({})",
                    signoff.approval_id,
                    reason_text(signoff.reason)
                ),
            ),
            Event::Answer(answer) => self.line(
                "answer",
                &format!(
                    "confidence {}, {}",
                    p(answer.confidence),
                    count(answer.facts.len(), "fact")
                ),
            ),
            Event::Done(done) => self.line(
                "done",
                &format!(
                    "{:.1} s · ${:.4} · judge: {} · composer: {} · audit {}",
                    done.duration_ms as f64 / 1000.0,
                    done.cost_usd.get(),
                    name(&done.judge_mode),
                    name(&done.composer),
                    done.audit_id
                ),
            ),
            Event::Error(error) => self.line(
                "error",
                &format!(
                    "{}: {} · {}",
                    name(&error.code),
                    error.message,
                    spent(error.cost_usd, error.judge_mode)
                ),
            ),
        }
    }
}

/// What a question cost up to an ending other than `done`, and which judge answered:
/// `$0.0011 · judge: jev`, or `no judge asked` when the question ended before any.
fn spent(cost: Usd, judge: Option<JudgeMode>) -> String {
    let judge = judge.map_or_else(
        || "no judge asked".to_owned(),
        |mode| format!("judge: {}", name(&mode)),
    );
    format!("${:.4} · {judge}", cost.get())
}

/// The result of `mercury ask`, once the events are done. A failure is reported as an error
/// instead, on standard error.
pub fn result(summary: &Summary, out: &mut dyn Write) -> io::Result<()> {
    match summary.status {
        QuestionStatus::Delivered => match &summary.answer {
            Some(answer) => answer_text(answer, out),
            None => Ok(()),
        },
        QuestionStatus::Refused => match &summary.refusal {
            Some(refusal) => {
                writeln!(out, "Refused: {}", block(&refusal.message))?;
                writeln!(out, "{}", block(&refusal.hint))?;
                if refusal.code == RefusalCode::RejectedByApprover {
                    writeln!(
                        out,
                        "Ask again, in new words or the same, naming the rejected question:\n  mercury ask \"<your question>\" --follows {}",
                        summary.id
                    )?;
                }
                Ok(())
            }
            None => Ok(()),
        },
        QuestionStatus::Clarify => match &summary.clarify {
            Some(clarify) => {
                writeln!(out, "{}", block(&clarify.question))?;
                let width = clarify
                    .options
                    .iter()
                    .map(|o| one_line(&o.id).len())
                    .max()
                    .unwrap_or(0);
                for option in &clarify.options {
                    writeln!(
                        out,
                        "  {:<width$}  {}",
                        one_line(&option.id),
                        one_line(&option.label)
                    )?;
                }
                let example = clarify
                    .options
                    .first()
                    .map_or("<option>".to_owned(), |o| one_line(&o.id));
                writeln!(
                    out,
                    "Ask again with the same question and the option you pick:\n  mercury ask \"<your question>\" --clarify {}:{example}",
                    summary.id
                )
            }
            None => Ok(()),
        },
        QuestionStatus::PendingSignoff => match &summary.signoff {
            Some(signoff) => {
                writeln!(
                    out,
                    "The answer waits for an approver ({}); approval {}.",
                    reason_text(signoff.reason),
                    signoff.approval_id
                )?;
                writeln!(out, "Follow it with:\n  mercury ask --wait {}", summary.id)
            }
            None => Ok(()),
        },
        QuestionStatus::Running | QuestionStatus::Failed => Ok(()),
    }
}

fn answer_text(answer: &Answer, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}", block(&answer.text))?;
    if answer.facts.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nFacts ({}):", answer.facts.len())?;
    for fact in &answer.facts {
        writeln!(out, "  · {}", fact_text(fact))?;
    }
    Ok(())
}

fn fact_text(fact: &Fact) -> String {
    let mut parts = vec![
        one_line(fact.title.as_str()),
        one_line(fact.module.as_str()),
        words(&fact.status),
        format!("tests {}", words(&fact.tested)),
    ];
    if let Some(waiting) = &fact.waiting_on {
        parts.push(format!("waiting on {}", words(waiting)));
    }
    parts.push(format!("updated {}", date(&fact.updated_at)));
    parts.join(" · ")
}

/// `mercury login`, once the token is kept.
pub fn signed_in(me: &Me, server: &Server, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "Signed in to {server} as {} ({}). The token is in the OS keychain.",
        one_line(&me.user.display_name),
        words(&me.user.role)
    )
}

/// `mercury me`.
pub fn me(me: &Me, server: &Server, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "Signed in to {server} as {} ({}), {}",
        one_line(&me.user.display_name),
        words(&me.user.role),
        me.user.email
    )?;
    writeln!(
        out,
        "Token {}, valid until {}",
        me.token.public_id,
        moment(&me.token.expires_at)
    )?;
    let projects: Vec<String> = me
        .projects
        .iter()
        .map(|grant| {
            if grant.approver {
                format!("{} (approver)", grant.project)
            } else {
                grant.project.to_string()
            }
        })
        .collect();
    let projects = if projects.is_empty() {
        "none".to_owned()
    } else {
        projects.join(", ")
    };
    writeln!(out, "Projects: {projects}")
}

/// `mercury projects`.
pub fn projects(list: &ProjectList, out: &mut dyn Write) -> io::Result<()> {
    if list.projects.is_empty() {
        return writeln!(out, "No projects yet: ask the admin for access.");
    }
    for (n, project) in list.projects.iter().enumerate() {
        if n > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{}  {}", project.project, one_line(&project.display_name))?;
        let probe = match &project.probe {
            Some(probe) => format!("{}, data as of {}", words(&probe.status), moment(&probe.as_of)),
            None => "not registered yet".to_owned(),
        };
        writeln!(out, "  probe    {probe}")?;
        let classes: Vec<String> = project.classes.iter().map(name).collect();
        writeln!(out, "  classes  {}", list_or_none(&classes))?;
        let modules: Vec<String> = project
            .modules
            .iter()
            .map(|module| one_line(module.as_str()))
            .collect();
        writeln!(out, "  modules  {}", list_or_none(&modules))?;
    }
    Ok(())
}

/// `mercury audit`: one line per row, oldest first; `more` when the server has more than shown.
pub fn audit(items: &[AuditItem], more: bool, out: &mut dyn Write) -> io::Result<()> {
    if items.is_empty() {
        return writeln!(out, "No audit rows to show.");
    }
    let rows: Vec<[String; 6]> = items
        .iter()
        .map(|item| {
            let kind = if item.payload.is_some() {
                item.event_type.as_str().to_owned()
            } else {
                format!("{} (payload expired)", item.event_type.as_str())
            };
            [
                item.seq.to_string(),
                item.at.format("%Y-%m-%d %H:%M:%S").to_string(),
                kind,
                item.project
                    .as_ref()
                    .map_or("-".to_owned(), |p| p.as_str().to_owned()),
                item.question_id
                    .as_ref()
                    .map_or("-".to_owned(), |q| q.as_str().to_owned()),
                item.user
                    .as_ref()
                    .map_or("-".to_owned(), |u| one_line(u.as_str())),
            ]
        })
        .collect();
    let header = ["SEQ", "TIME (UTC)", "TYPE", "PROJECT", "QUESTION", "USER"];
    let mut widths = header.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>, out: &mut dyn Write| -> io::Result<()> {
        let mut text = String::new();
        for (n, (cell, width)) in cells.iter().zip(widths).enumerate() {
            if n + 1 == cells.len() {
                text.push_str(cell);
            } else {
                text.push_str(&format!("{cell:<width$}  "));
            }
        }
        writeln!(out, "{}", text.trim_end())
    };
    line(header.to_vec(), out)?;
    for row in &rows {
        line(row.iter().map(String::as_str).collect(), out)?;
    }
    if more {
        writeln!(
            out,
            "More rows than these {}: narrow them with --project or --since, or export them with --export.",
            items.len()
        )?;
    }
    Ok(())
}

/// One JSON object and a newline.
pub fn json<T: Serialize>(value: &T, out: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value).map_err(io::Error::other)?;
    writeln!(out)
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

fn check_text(check: &CheckResult) -> String {
    let source = match check.p {
        Some(ratio) => format!("{}, p {}", name(&check.source), p(ratio)),
        None => name(&check.source),
    };
    format!("{} {} ({source})", check.id, name(&check.verdict))
}

/// `progress(limit 20, since 2026-09-28)`, the arguments by name. The code reader's call names
/// its argument only, `ask_code(question)`: the line never repeats the question.
fn call_text(call: &ToolCall) -> String {
    if let ToolCall::AskCode(_) = call {
        return format!("{}(question)", call.tool().as_str());
    }
    let mut args: Vec<String> = match serde_json::to_value(call)
        .ok()
        .and_then(|mut value| value.get_mut("args").map(Value::take))
    {
        Some(Value::Object(args)) => args
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => format!("{key} {}", one_line(text)),
                other => format!("{key} {other}"),
            })
            .collect(),
        _ => Vec::new(),
    };
    args.sort_unstable();
    format!("{}({})", call.tool().as_str(), args.join(", "))
}

/// `1 fact`, `3 facts`.
fn count(n: usize, thing: &str) -> String {
    if n == 1 {
        format!("1 {thing}")
    } else {
        format!("{n} {thing}s")
    }
}

/// `mercury approvals`: each held answer with why it waits, what it would say and what was
/// flagged in it, oldest first. The caller's own question shows only that it waits for another
/// approver: an asker never sees their held draft, nor what was flagged in it, before the decision.
/// The server leaves both out of it; were one to send them, they would still not be shown.
pub fn approvals(list: &ApprovalList, out: &mut dyn Write) -> io::Result<()> {
    if list.approvals.is_empty() {
        return writeln!(out, "No answers wait for your sign-off.");
    }
    for (n, item) in list.approvals.iter().enumerate() {
        if n > 0 {
            writeln!(out)?;
        }
        let project = item
            .project
            .as_ref()
            .map_or("across projects".to_owned(), |p| p.as_str().to_owned());
        writeln!(
            out,
            "{} · {project} · {} · asked by a {} · until {}",
            item.id,
            words(&item.class),
            words(&item.asker_role),
            moment(&item.expires_at)
        )?;
        writeln!(out, "  Waits because {}.", reason_text(item.reason))?;
        if item.asked_by_me {
            writeln!(
                out,
                "  Your own question: it waits for another approver of the project, and only they see the draft."
            )?;
            writeln!(out, "  Follow it with: mercury ask --wait {}", item.question_id)?;
            continue;
        }
        // nobody approves what they haven't read
        let Some(draft) = &item.draft else {
            writeln!(
                out,
                "  The list came without its draft: list the answers again before you decide."
            )?;
            continue;
        };
        writeln!(out, "  Draft: {}", one_line(&draft.text))?;
        for flag in item.flags.iter().flatten() {
            writeln!(
                out,
                "  Flagged: \"{}\" ({}, p {})",
                one_line(&flag.sentence),
                flag.check.as_str(),
                p(flag.p)
            )?;
        }
        writeln!(
            out,
            "  Approve with: mercury approvals --approve {} · reject with: mercury approvals --reject {} --note \"…\"",
            item.id, item.id
        )?;
    }
    Ok(())
}

/// `mercury approvals --json`: the server's list, the caller's own questions without their draft
/// and flags, as the server sends them and the list in words shows them.
pub fn approvals_json(list: &ApprovalList, out: &mut dyn Write) -> io::Result<()> {
    let mut value = serde_json::to_value(list).map_err(io::Error::other)?;
    if let Some(items) = value["approvals"].as_array_mut() {
        for item in items.iter_mut().filter(|item| item["asked_by_me"] == true) {
            if let Some(item) = item.as_object_mut() {
                item.remove("draft");
                item.remove("flags");
            }
        }
    }
    json(&value, out)
}

/// What a decision did.
pub fn decided(decision: Decision, out: &mut dyn Write) -> io::Result<()> {
    match decision {
        Decision::Approve => writeln!(
            out,
            "Approved: the asker gets the answer exactly as the list showed it."
        ),
        Decision::Reject => writeln!(out, "Rejected: the asker gets the refusal with your note."),
    }
}

fn reason_text(reason: SignoffReason) -> &'static str {
    match reason {
        SignoffReason::Class => "the class needs sign-off",
        SignoffReason::ReviewBand => "an output check landed in its review band",
    }
}

fn p(ratio: Ratio) -> String {
    format!("{:.2}", ratio.get())
}

/// `, lowest p 0.91` when a judge answered a stage's checks; nothing when only rules decided.
fn lowest(p_min: Option<Ratio>) -> String {
    p_min.map_or_else(String::new, |ratio| format!(", lowest p {}", p(ratio)))
}

fn date(at: &DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

fn moment(at: &DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// A value's name on the wire, `asks_for_code`.
fn name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => "?".to_owned(),
    }
}

/// A value's name in words, `in review`.
fn words<T: Serialize>(value: &T) -> String {
    name(value).replace('_', " ")
}

/// Server text for one line: no control characters, line breaks as spaces.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// Server text over several lines: line breaks and tabs stay, other control characters go.
fn block(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn timeline(events: &[serde_json::Value]) -> String {
        let mut out = Vec::new();
        let mut view = Timeline::new(&mut out);
        for value in events {
            let event: Event = serde_json::from_value(value.clone()).unwrap();
            view.event(&event).unwrap();
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_timeline_names_each_step() {
        let text = timeline(&[
            json!({ "event": "project", "data": { "project": "delta", "p": 0.91, "source": "judge" } }),
            json!({ "event": "classified", "data": { "class": "progress", "p": 0.94, "checks": [
                { "id": "asks_for_code", "source": "jev", "p": 0.02, "verdict": "pass" },
                { "id": "contains_agent_instruction", "source": "rule", "verdict": "pass" } ] } }),
            json!({ "event": "plan", "data": { "calls": [
                { "tool": "progress", "args": { "since": "2026-09-28", "limit": 20 } }, { "tool": "health", "args": {} } ] } }),
            json!({ "event": "tool_checked", "data": { "call": 0, "checks": [
                { "id": "arguments_valid", "source": "rule", "verdict": "pass" } ] } }),
            json!({ "event": "tool_result", "data": { "call": 0, "items": 12, "withheld_fields": ["/items/3/title"], "access": "code" } }),
            json!({ "event": "judged", "data": { "sentences": 6, "stripped": 1, "held": 0 } }),
            json!({ "event": "done", "data": { "duration_ms": 6420, "cost_usd": 0.011, "judge_mode": "jev", "composer": "model", "audit_id": 1834 } }),
        ]);
        let expected = "\
project    delta (picked by the judge, p 0.91)
class      progress (p 0.94)
check      asks_for_code pass (jev, p 0.02)
check      contains_agent_instruction pass (rule)
plan       progress(limit 20, since 2026-09-28) · health()
call 1     progress: arguments_valid pass (rule)
result 1   progress: 12 items, access code, 1 value withheld
judged     6 sentences, 1 stripped, 0 held for sign-off
done       6.4 s · $0.0110 · judge: jev · composer: model · audit 1834
";
        assert_eq!(text, expected);
    }

    /// The CLI has no words of its own for a refusal: the timeline names its reason by its code, and
    /// the result prints the core's message and its hint.
    #[test]
    fn a_confidential_refusal_prints_its_reason_and_hint() {
        let refused = json!({ "event": "refused", "data": {
            "code": "confidential",
            "message": "Mercury doesn't answer about confidential matters.",
            "hint": "Contracts, prices and salaries stay out of it: ask about the work in Export instead.",
            "cost_usd": 0.0012, "judge_mode": "jev" } });
        assert_eq!(
            timeline(std::slice::from_ref(&refused)),
            "refused    confidential · $0.0012 · judge: jev\n"
        );

        let Event::Refused(refusal) = serde_json::from_value(refused).unwrap() else {
            panic!("a refused event")
        };
        let mut summary = Summary::new(QuestionId::parse("q_01J9Z3QK7M8N9P0R1S2T3V0001").unwrap());
        summary.status = QuestionStatus::Refused;
        summary.refusal = Some(refusal);
        let mut out = Vec::new();
        result(&summary, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Refused: Mercury doesn't answer about confidential matters.\n\
             Contracts, prices and salaries stay out of it: ask about the work in Export instead.\n"
        );
    }

    /// A stage a judge checked says the lowest probability that it is fine; one only the rules
    /// decided says nothing of it.
    #[test]
    fn a_stage_says_its_lowest_probability_when_a_judge_gave_one() {
        let text = timeline(&[
            json!({ "event": "plan", "data": { "calls": [
                { "tool": "progress", "args": {} }, { "tool": "health", "args": {} } ] } }),
            json!({ "event": "tool_result", "data": { "call": 0, "items": 12, "withheld_fields": ["/items/3/title"],
                "access": "code", "p_min": 0.91 } }),
            json!({ "event": "tool_result", "data": { "call": 1, "items": 1, "withheld_fields": [], "access": "code" } }),
            json!({ "event": "judged", "data": { "sentences": 6, "stripped": 1, "held": 0, "p_min": 0.874 } }),
            json!({ "event": "judged", "data": { "sentences": 2, "stripped": 0, "held": 0 } }),
        ]);
        let expected = "\
plan       progress() · health()
result 1   progress: 12 items, access code, 1 value withheld, lowest p 0.91
result 2   health: 1 item, access code
judged     6 sentences, 1 stripped, 0 held for sign-off, lowest p 0.87
judged     2 sentences, 0 stripped, 0 held for sign-off
";
        assert_eq!(text, expected);
    }

    /// The code reader's call names its argument and never repeats the question, which may be up
    /// to a thousand characters; its result is counted like any other.
    #[test]
    fn the_code_readers_call_doesnt_repeat_the_question() {
        let question = "Where does the export build the names of its files?";
        let text = timeline(&[
            json!({ "event": "plan", "data": { "calls": [
                { "tool": "progress", "args": { "limit": 20 } },
                { "tool": "ask_code", "args": { "question": question } } ] } }),
            json!({ "event": "tool_result", "data": { "call": 1, "items": 2, "withheld_fields": [],
                "access": "code_model", "p_min": 0.96 } }),
        ]);
        let expected = "\
plan       progress(limit 20) · ask_code(question)
result 2   ask_code: 2 items, access code_model, lowest p 0.96
";
        assert_eq!(text, expected);
        assert!(!text.contains("export"), "{text}");
    }

    /// OpenRouter down: the rules judged and the template wrote, and the last line says both.
    #[test]
    fn a_degraded_answer_says_the_rules_judged_and_the_template_wrote_it() {
        let text = timeline(&[json!({ "event": "done", "data": {
            "duration_ms": 180, "cost_usd": 0.0, "judge_mode": "rules", "composer": "template", "audit_id": 41 } })]);
        assert_eq!(
            text,
            "done       0.2 s · $0.0000 · judge: rules · composer: template · audit 41\n"
        );
    }

    /// Every ending says what the question cost, and which judge answered when one did.
    #[test]
    fn every_ending_says_what_it_cost_and_which_judge_answered() {
        let text = timeline(&[
            json!({ "event": "refused", "data": { "code": "asks_for_code", "message": "No code.",
                "hint": "Ask what is done.", "cost_usd": 0.0011, "judge_mode": "jev" } }),
            json!({ "event": "refused", "data": { "code": "forbidden_project", "message": "Not yours.",
                "hint": "Ask about delta.", "cost_usd": 0.0, "judge_mode": null } }),
            json!({ "event": "clarify", "data": { "kind": "project", "question": "Which project?",
                "options": [{ "id": "delta", "label": "Delta" }, { "id": "kappa", "label": "Kappa" }],
                "cost_usd": 0.0009, "judge_mode": "chat" } }),
            json!({ "event": "error", "data": { "code": "budget_exceeded",
                "message": "The question reached its spending limit, so it was stopped.",
                "cost_usd": 0.051, "judge_mode": "jev" } }),
        ]);
        let expected = "\
refused    asks_for_code · $0.0011 · judge: jev
refused    forbidden_project · $0.0000 · no judge asked
clarify    Which project? (2 options) · $0.0009 · judge: chat
error      budget_exceeded: The question reached its spending limit, so it was stopped. · $0.0510 · judge: jev
";
        assert_eq!(text, expected);
    }

    /// The server leaves the draft and flags out of the caller's own question; one that sent them
    /// anyway still has nothing of them shown, in words or in JSON. Someone else's item without a
    /// draft offers no way to approve it.
    #[test]
    fn an_own_question_shows_nothing_of_its_draft_whatever_the_server_sends() {
        let item = |id: &str, mine: bool, draft: bool| {
            let mut item = json!({
                "id": id, "question_id": "q_01J9Z3QK7M8N9P0R1S2T3V0001", "project": "delta",
                "class": "progress", "asker_role": "pm", "reason": "class",
                "asked_by_me": mine, "status": "pending",
                "created_at": "2026-10-04T15:12:09Z", "expires_at": "2026-10-05T15:12:09Z"
            });
            if draft {
                item["draft"] =
                    json!({ "text": "Auth waits on the SsoBridge review.", "facts": [], "confidence": 0.8 });
                item["flags"] = json!([{ "sentence": "Auth waits on the SsoBridge review.",
                    "check": "sentence_contains_code_or_identifier", "p": 0.6 }]);
            }
            item
        };
        let list: ApprovalList = serde_json::from_value(json!({ "approvals": [
            item("ap_01J9Z3QK7M8N9P0R1S2T3V0001", true, true),
            item("ap_01J9Z3QK7M8N9P0R1S2T3V0002", false, false),
        ] }))
        .unwrap();

        let mut text = Vec::new();
        approvals(&list, &mut text).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(!text.contains("SsoBridge"), "{text}");
        assert!(text.contains("Your own question"), "{text}");
        assert!(
            text.contains("list the answers again before you decide"),
            "{text}"
        );
        assert!(!text.contains("--approve"), "nothing to approve unread: {text}");

        let mut out = Vec::new();
        approvals_json(&list, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains("SsoBridge"), "{out}");
        let value: Value = serde_json::from_str(&out).unwrap();
        let own = value["approvals"][0].as_object().unwrap();
        assert!(
            !own.contains_key("draft") && !own.contains_key("flags"),
            "{own:?}"
        );
    }

    #[test]
    fn server_text_cant_steer_the_terminal() {
        let text = timeline(&[json!({ "event": "error", "data": {
            "code": "internal", "message": "bad\u{1b}[2J\nthing\u{7}", "cost_usd": 0.0, "judge_mode": null } })]);
        assert_eq!(
            text,
            "error      internal: bad[2J thing · $0.0000 · no judge asked\n"
        );
        assert_eq!(block("one\ntwo\u{1b}"), "one\ntwo");
    }
}
