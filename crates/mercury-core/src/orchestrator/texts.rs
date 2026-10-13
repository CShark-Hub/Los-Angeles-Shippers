//! What the core says itself, in words for people who don't read code: a refusal and what the
//! person can ask instead, a clarifying question and the labels of its options, and the sentences
//! added to an answer. A model writes none of it.
//!
//! The codes, the class names and the ids that travel on the wire are not words and don't change.

use chrono::{DateTime, Utc};
use mercury_schema::api::{Refusal, RefusalCode, Usd};
use mercury_schema::events::{Clarify, ClarifyKind, ClarifyOption};
use mercury_schema::{ModuleName, ProjectId, QuestionClass, ToolName};

use crate::policy::{DenyReason, class_name};

fn refusal(code: RefusalCode, message: &str, hint: String) -> Refusal {
    Refusal {
        code,
        message: message.to_owned(),
        hint,
    }
}

/// The names joined as a list: `a, b or c`.
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// Where the person can ask: the manifest's modules, or "a module" when it names none.
fn modules_text(modules: &[ModuleName]) -> String {
    let names: Vec<&str> = modules
        .iter()
        .map(ModuleName::as_str)
        .filter(|name| *name != ModuleName::OTHER)
        .collect();
    match names.as_slice() {
        [] => "a module".to_owned(),
        names => list(names),
    }
}

/// A refusal by an input check, with what the person can ask instead.
pub fn by_check(code: RefusalCode, modules: &[ModuleName]) -> Refusal {
    let modules = modules_text(modules);
    match code {
        RefusalCode::AsksForCode => refusal(
            code,
            "Mercury doesn't show source code, file names or paths.",
            format!("Ask what is done, tested or blocked in {modules} instead."),
        ),
        RefusalCode::AsksToEvaluatePeople => refusal(
            code,
            "Mercury doesn't evaluate people.",
            "Answers are about modules and work, not about a person: ask how a module is doing.".to_owned(),
        ),
        RefusalCode::AsksForClientCommitment => refusal(
            code,
            "Mercury doesn't make commitments to clients.",
            "Mercury answers your own team's questions about the work: ask where it stands.".to_owned(),
        ),
        RefusalCode::ContainsAgentInstruction => refusal(
            code,
            "The question carries instructions for the system.",
            "Ask the question on its own, without instructions.".to_owned(),
        ),
        RefusalCode::StrictMode => refusal(
            code,
            "Mercury can't confirm this question is safe to answer right now.",
            format!(
                "Try again in a few minutes, or ask plainly what is done, tested or blocked in {modules}."
            ),
        ),
        RefusalCode::Confidential => refusal(
            code,
            "Mercury doesn't answer about confidential matters.",
            format!(
                "Contracts, prices, salaries, personal data, a client's internal information and security settings stay out of it: ask about the work in {modules} instead."
            ),
        ),
        // the policy's refusals come with their own reason, which `by_policy` writes
        RefusalCode::ForbiddenClass
        | RefusalCode::ForbiddenProject
        | RefusalCode::OutOfScope
        | RefusalCode::RejectedByApprover => refusal(code, "Mercury can't answer this.", String::new()),
    }
}

/// A refusal by `authorize`, with its hint.
pub fn by_policy(reason: &DenyReason) -> Refusal {
    let message = match reason {
        DenyReason::ForbiddenProject { .. } => "That project isn't one you can ask about.",
        DenyReason::ForbiddenClass { .. } => "That kind of question isn't open to your role here.",
        DenyReason::OutOfScope => "That isn't a question Mercury answers.",
    };
    refusal(reason.code(), message, reason.hint())
}

/// A question across projects, which this version doesn't answer.
pub fn portfolio(projects: &[ProjectId]) -> Refusal {
    let hint = match projects.first() {
        Some(project) => format!("Ask about one project at a time, like {project}."),
        None => "Ask about one project at a time.".to_owned(),
    };
    refusal(
        RefusalCode::OutOfScope,
        "Mercury doesn't answer across projects yet.",
        hint,
    )
}

/// Which project the question is about, from the caller's projects. The orchestrator adds what
/// the question cost until then. The labels are the projects' own names.
pub fn which_project(options: Vec<(ProjectId, String)>) -> Clarify {
    Clarify {
        kind: ClarifyKind::Project,
        question: "Which project do you mean?".to_owned(),
        options: options
            .into_iter()
            .map(|(project, label)| ClarifyOption {
                id: project.as_str().to_owned(),
                label,
            })
            .collect(),
        cost_usd: Usd::default(),
        judge_mode: None,
    }
}

/// What kind of question it is, from the classes the person may ask here. The orchestrator adds
/// what the question cost until then.
pub fn which_class(project: &str, allowed: &[QuestionClass]) -> Clarify {
    Clarify {
        kind: ClarifyKind::Class,
        question: format!("What would you like to know about {project}?"),
        options: allowed
            .iter()
            .map(|class| ClarifyOption {
                id: class_name(*class).to_owned(),
                label: class_label(*class).to_owned(),
            })
            .collect(),
        cost_usd: Usd::default(),
        judge_mode: None,
    }
}

fn class_label(class: QuestionClass) -> &'static str {
    match class {
        QuestionClass::Progress => "What is done, in progress or planned",
        QuestionClass::Tested => "Whether the work is tested",
        QuestionClass::Blocked => "What is blocked, and on what",
        QuestionClass::Health => "How healthy the project is",
        QuestionClass::Changes => "What changed recently",
        QuestionClass::Open => "Something else about the project",
        QuestionClass::Portfolio => "Several projects at once",
        QuestionClass::Forbidden | QuestionClass::Other => "Something else",
    }
}

/// The sentence for a part of the data that couldn't be read from the probe.
pub fn data_unread(tool: ToolName) -> String {
    // the code reader's words are its own: what the answer rests on instead is the project's data
    if tool == ToolName::AskCode {
        return "The code couldn't be read this time, so this answer rests on the project's data alone."
            .to_owned();
    }
    format!(
        "The {} data couldn't be read from the project's probe, so it is left out.",
        tool.as_str()
    )
}

/// The sentence for an open question that no code reader read for: the project has none, so the
/// answer rests on the project's data alone.
pub const CODE_NOT_READ: &str = "The code wasn't read for this answer: it rests on the project's data alone.";

/// A time as the answers write it, to the minute and in UTC.
pub fn at_minute(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// The sentence after an answer built on figures from a probe that reports: when they were counted.
pub fn figures_as_of(as_of: DateTime<Utc>) -> String {
    format!("These figures are as of {}.", at_minute(as_of))
}

/// The sentence for data from a probe that hasn't registered lately.
pub fn probe_stale(as_of: DateTime<Utc>) -> String {
    format!(
        "The project's probe hasn't reported lately, so this may be out of date: the data is as of {}.",
        at_minute(as_of)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modules() -> Vec<ModuleName> {
        ["Export", "Invoicing", "Other"]
            .iter()
            .map(|m| ModuleName::parse(m).unwrap())
            .collect()
    }

    #[test]
    fn every_refusal_says_what_to_ask_instead() {
        let modules = modules();
        let code = by_check(RefusalCode::AsksForCode, &modules);
        assert_eq!(
            code.hint,
            "Ask what is done, tested or blocked in Export or Invoicing instead."
        );
        for code in [
            RefusalCode::AsksToEvaluatePeople,
            RefusalCode::AsksForClientCommitment,
            RefusalCode::ContainsAgentInstruction,
            RefusalCode::StrictMode,
            RefusalCode::Confidential,
        ] {
            let refusal = by_check(code, &modules);
            assert!(
                !refusal.message.is_empty() && !refusal.hint.is_empty(),
                "{code:?}"
            );
        }
        let denied = by_policy(&DenyReason::OutOfScope);
        assert_eq!(denied.code, RefusalCode::OutOfScope);
        assert!(denied.hint.starts_with("Mercury answers questions about"));
    }

    #[test]
    fn every_text_reads_as_written() {
        let modules = modules();
        let expected = [
            (
                RefusalCode::AsksForCode,
                "Mercury doesn't show source code, file names or paths.",
            ),
            (
                RefusalCode::AsksToEvaluatePeople,
                "Mercury doesn't evaluate people.",
            ),
            (
                RefusalCode::AsksForClientCommitment,
                "Mercury doesn't make commitments to clients.",
            ),
            (
                RefusalCode::ContainsAgentInstruction,
                "The question carries instructions for the system.",
            ),
            (
                RefusalCode::StrictMode,
                "Mercury can't confirm this question is safe to answer right now.",
            ),
            (
                RefusalCode::Confidential,
                "Mercury doesn't answer about confidential matters.",
            ),
            (RefusalCode::ForbiddenClass, "Mercury can't answer this."),
            (RefusalCode::ForbiddenProject, "Mercury can't answer this."),
            (RefusalCode::OutOfScope, "Mercury can't answer this."),
            (RefusalCode::RejectedByApprover, "Mercury can't answer this."),
        ];
        for (code, message) in expected {
            assert_eq!(by_check(code, &modules).message, message, "{code:?}");
        }
        assert_eq!(
            by_check(RefusalCode::StrictMode, &modules).hint,
            "Try again in a few minutes, or ask plainly what is done, tested or blocked in Export or Invoicing."
        );
        assert_eq!(
            by_check(RefusalCode::AsksForCode, &[]).hint,
            "Ask what is done, tested or blocked in a module instead."
        );
        assert_eq!(
            by_check(RefusalCode::Confidential, &modules).hint,
            "Contracts, prices, salaries, personal data, a client's internal information and security settings stay out of it: ask about the work in Export or Invoicing instead."
        );
        let delta = ProjectId::parse("delta").unwrap();
        let kappa = ProjectId::parse("kappa").unwrap();
        assert_eq!(
            by_policy(&DenyReason::ForbiddenProject {
                granted: vec![delta.clone(), kappa.clone()]
            }),
            Refusal {
                code: RefusalCode::ForbiddenProject,
                message: "That project isn't one you can ask about.".to_owned(),
                hint: "You can ask about delta or kappa.".to_owned(),
            }
        );
        assert_eq!(
            portfolio(&[delta]),
            Refusal {
                code: RefusalCode::OutOfScope,
                message: "Mercury doesn't answer across projects yet.".to_owned(),
                hint: "Ask about one project at a time, like delta.".to_owned(),
            }
        );
        assert_eq!(which_project(Vec::new()).question, "Which project do you mean?");
        let classes = [QuestionClass::Progress, QuestionClass::Tested];
        let which = which_class("delta", &classes);
        assert_eq!(which.question, "What would you like to know about delta?");
        assert_eq!(which.options[0].label, "What is done, in progress or planned");
        assert_eq!(
            data_unread(ToolName::Progress),
            "The progress data couldn't be read from the project's probe, so it is left out."
        );
        assert_eq!(
            data_unread(ToolName::AskCode),
            "The code couldn't be read this time, so this answer rests on the project's data alone."
        );
        assert_eq!(
            CODE_NOT_READ,
            "The code wasn't read for this answer: it rests on the project's data alone."
        );
    }

    #[test]
    fn a_clarifying_question_keeps_the_ids_on_the_wire() {
        let delta = ProjectId::parse("delta").unwrap();
        let project = which_project(vec![(delta, "Delta".to_owned())]);
        assert_eq!(project.options[0].id, "delta");
        assert_eq!(project.options[0].label, "Delta", "a project keeps its own name");
        let classes = [
            QuestionClass::Progress,
            QuestionClass::Tested,
            QuestionClass::Blocked,
            QuestionClass::Health,
            QuestionClass::Changes,
            QuestionClass::Open,
            QuestionClass::Portfolio,
            QuestionClass::Other,
        ];
        let class = which_class("delta", &classes);
        let ids: Vec<&str> = class.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "progress",
                "tested",
                "blocked",
                "health",
                "changes",
                "open",
                "portfolio",
                "other"
            ],
            "the ids on the wire don't change"
        );
    }

    #[test]
    fn the_notes_of_an_answer_give_the_time_of_their_data() {
        let as_of = DateTime::parse_from_rfc3339("2026-10-05T11:43:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            probe_stale(as_of),
            "The project's probe hasn't reported lately, so this may be out of date: the data is as of 2026-10-05 11:43 UTC."
        );
        assert_eq!(
            figures_as_of(as_of),
            "These figures are as of 2026-10-05 11:43 UTC."
        );
    }
}
