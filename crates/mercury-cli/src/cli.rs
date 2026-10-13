//! The command line. The exit codes and the environment variables are part of the contract with
//! scripts, so `--help` names them.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use clap::{ArgGroup, Args, Parser, Subcommand};
use mercury_schema::api::ClarifyAnswer;
use mercury_schema::ids::{ApprovalId, ProjectId, QuestionId};

pub const AFTER_HELP: &str = "\
Exit codes, which scripts and agents rely on:
  0  answered
  1  an error, a wrong command line included
  2  refused; the reason and a hint are printed (after an approver's rejection, ask again with
     --follows <question id>)
  3  a clarifying question; ask again with --clarify <question id>:<option>
  4  waiting for an approver's sign-off; follow it with --wait <question id>

Without a keychain (CI, containers), set MERCURY_URL and MERCURY_TOKEN. With MERCURY_TOKEN set,
the keychain isn't used.

Asking stops waiting after MERCURY_ASK_DEADLINE seconds (120 by default, at most a day): it exits
1 with the question's state as it stands, so a script never hangs. The question may still be
running; follow it with --wait, which has no deadline.";

#[derive(Debug, Parser)]
#[command(
    name = "mercury",
    version,
    about = "Ask Mercury what a project has done, tested and is blocked on, how healthy it is and what changed",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in: check the token with the server, then keep it in the OS keychain
    Login {
        /// The Mercury server, like https://mercury.example.com
        #[arg(long, value_name = "URL")]
        url: String,
        /// The token. Without it, the token is read from standard input, which keeps it out of
        /// the shell's history
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
    /// Sign out: remove the token from the OS keychain
    Logout,
    /// Who the token belongs to: name, role and projects
    Me {
        /// Print the server's JSON instead
        #[arg(long)]
        json: bool,
    },
    /// The projects you may ask about, with their modules and question classes
    Projects {
        /// Print the server's JSON instead
        #[arg(long)]
        json: bool,
    },
    /// Ask a question: the timeline goes to standard error, the result to standard output
    #[command(after_help = AFTER_HELP)]
    Ask(AskArgs),
    /// The audit trail, for an admin: one line per event, oldest first. Anyone else is refused;
    /// what Mercury did with your own question is in `ask`'s timeline
    Audit(AuditArgs),
    /// The answers waiting for your sign-off in the projects you approve, with what was flagged;
    /// or release one unchanged, or reject it with a note for the asker
    Approvals(ApprovalsArgs),
}

impl Command {
    /// Whether standard output is for one JSON object.
    pub fn json(&self) -> bool {
        match self {
            Command::Me { json } | Command::Projects { json } => *json,
            Command::Ask(args) => args.json,
            Command::Approvals(args) => args.json,
            Command::Login { .. } | Command::Logout | Command::Audit(_) => false,
        }
    }
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// The question, in plain words
    #[arg(required_unless_present = "wait")]
    pub text: Option<String>,
    /// The project; without it, the judge picks one of yours or asks which
    #[arg(long, value_name = "PROJECT", value_parser = project, conflicts_with = "wait")]
    pub project: Option<ProjectId>,
    /// Answer a clarifying question: its question id and the option you pick
    #[arg(long, value_name = "QUESTION_ID:OPTION", value_parser = clarify, conflicts_with = "wait")]
    pub clarify: Option<ClarifyAnswer>,
    /// Ask again, in new words or the same, a question of yours that an approver rejected: its
    /// question id. The audit links the two; the new question is checked like any other
    #[arg(long, value_name = "QUESTION_ID", value_parser = question_id, conflicts_with = "wait")]
    pub follows: Option<QuestionId>,
    /// Follow a question asked before, one waiting for sign-off say, to its final state
    #[arg(long, value_name = "QUESTION_ID", value_parser = question_id, conflicts_with = "text")]
    pub wait: Option<QuestionId>,
    /// Print one JSON object with the final state on standard output
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AuditArgs {
    /// Only this project's rows
    #[arg(long, value_name = "PROJECT", value_parser = project)]
    pub project: Option<ProjectId>,
    /// Only the rows since then: a date (2026-10-01), a time (RFC 3339), or a span back from now
    /// (7d, 24h)
    #[arg(long, value_name = "WHEN", value_parser = since)]
    pub since: Option<DateTime<Utc>>,
    /// Write every matching row to FILE instead, one JSON object per line with the hashes that
    /// verify it; for admins
    #[arg(long, value_name = "FILE")]
    pub export: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("decision").args(["approve", "reject"])))]
pub struct ApprovalsArgs {
    /// Release this held answer to its asker, exactly as the list shows it
    #[arg(long, value_name = "APPROVAL_ID", value_parser = approval_id)]
    pub approve: Option<ApprovalId>,
    /// Refuse this held answer; the asker sees the note
    #[arg(long, value_name = "APPROVAL_ID", value_parser = approval_id, requires = "note")]
    pub reject: Option<ApprovalId>,
    /// What the asker should do instead, 1 to 500 characters; needed with --reject
    #[arg(long, value_name = "TEXT", requires = "decision")]
    pub note: Option<String>,
    /// Print the server's JSON list instead; your own questions come without their draft and flags
    #[arg(long, conflicts_with = "decision")]
    pub json: bool,
}

/// A date for its midnight UTC, an RFC 3339 time, or a span back from now.
fn since(raw: &str) -> Result<DateTime<Utc>, String> {
    let raw = raw.trim();
    let wrong = || {
        "give a date like 2026-10-01, a time like 2026-10-01T12:00:00Z, or a span like 7d or 24h".to_owned()
    };
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&Utc));
    }
    if let Ok(day) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return day
            .and_hms_opt(0, 0, 0)
            .map(|midnight| midnight.and_utc())
            .ok_or_else(wrong);
    }
    // the unit is the last character, which may take several bytes (`7é`): split at its start
    let Some((at, _)) = raw.char_indices().next_back() else {
        return Err(wrong());
    };
    let (number, unit) = raw.split_at(at);
    let count: i64 = number.parse().ok().filter(|n| *n > 0).ok_or_else(wrong)?;
    let span = match unit {
        "d" => TimeDelta::try_days(count),
        "h" => TimeDelta::try_hours(count),
        _ => None,
    }
    .ok_or_else(wrong)?;
    DateTime::<Utc>::from(std::time::SystemTime::now())
        .checked_sub_signed(span)
        .ok_or_else(wrong)
}

fn project(raw: &str) -> Result<ProjectId, String> {
    ProjectId::parse(raw)
        .map_err(|_| "a project id is lowercase letters, digits and dashes, like delta".to_owned())
}

fn approval_id(raw: &str) -> Result<ApprovalId, String> {
    ApprovalId::parse(raw).map_err(|_| "an approval id is ap_ and 26 letters and digits".to_owned())
}

fn question_id(raw: &str) -> Result<QuestionId, String> {
    QuestionId::parse(raw).map_err(|_| "a question id is q_ and 26 letters and digits".to_owned())
}

fn clarify(raw: &str) -> Result<ClarifyAnswer, String> {
    let (of, option) = raw
        .split_once(':')
        .ok_or_else(|| "give the question id and the option, like q_…:delta".to_owned())?;
    let option = option.trim();
    if option.is_empty() {
        return Err("the option is empty".to_owned());
    }
    Ok(ClarifyAnswer {
        of: question_id(of)?,
        option: option.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    const QUESTION: &str = "q_01J9Z3QK7M8N9P0R1S2T3V4W5X";

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mercury").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn audit_takes_a_project_a_since_and_an_export_file() {
        let Command::Audit(args) = parse(&[
            "audit",
            "--project",
            "delta",
            "--since",
            "2026-10-01",
            "--export",
            "a.jsonl",
        ])
        .unwrap()
        .command
        else {
            panic!("not audit");
        };
        assert_eq!(args.project.as_ref().map(ProjectId::as_str), Some("delta"));
        assert_eq!(args.since.unwrap().to_rfc3339(), "2026-10-01T00:00:00+00:00");
        assert_eq!(args.export.as_deref(), Some(std::path::Path::new("a.jsonl")));
        for good in ["7d", "24h", "2026-10-01T12:00:00Z"] {
            assert!(since(good).is_ok(), "{good}");
        }
        let week = since("7d").unwrap();
        let now = DateTime::<Utc>::from(std::time::SystemTime::now());
        assert!((now - week - TimeDelta::days(7)).num_seconds().abs() < 5);
        // a unit of several bytes is a mistake of the command line, never a panic
        for bad in [
            "yesterday",
            "0d",
            "7w",
            "-3d",
            "2026-13-01",
            "",
            "7é",
            "café",
            "7€",
            "é",
            "7📅",
        ] {
            assert!(since(bad).is_err(), "{bad}");
        }
        let help = parse(&["audit", "--help"]).unwrap_err().to_string();
        for flag in ["--project", "--since", "--export"] {
            assert!(help.contains(flag), "{flag} in {help}");
        }
    }

    #[test]
    fn ask_takes_a_question_or_a_question_to_wait_for() {
        let Command::Ask(args) = parse(&["ask", "what was merged?", "--project", "delta", "--json"])
            .unwrap()
            .command
        else {
            panic!("not ask");
        };
        assert_eq!(args.text.as_deref(), Some("what was merged?"));
        assert_eq!(args.project.as_ref().map(ProjectId::as_str), Some("delta"));
        assert!(args.json);

        let Command::Ask(args) = parse(&["ask", "--wait", QUESTION]).unwrap().command else {
            panic!("not ask");
        };
        assert_eq!(args.wait.as_ref().map(QuestionId::as_str), Some(QUESTION));

        assert!(parse(&["ask"]).is_err(), "a question or --wait");
        assert!(parse(&["ask", "what?", "--wait", QUESTION]).is_err());
        assert!(parse(&["ask", "--wait", QUESTION, "--project", "delta"]).is_err());
        assert!(parse(&["ask", "what?", "--project", "Delta"]).is_err());
    }

    #[test]
    fn a_clarify_answer_is_the_question_and_the_option() {
        let Command::Ask(args) = parse(&[
            "ask",
            "what was merged?",
            "--clarify",
            &format!("{QUESTION}:delta"),
        ])
        .unwrap()
        .command
        else {
            panic!("not ask");
        };
        let clarify = args.clarify.unwrap();
        assert_eq!(
            (clarify.of.as_str(), clarify.option.as_str()),
            (QUESTION, "delta")
        );
        for bad in ["delta", &format!("{QUESTION}:"), "q_1:delta"] {
            assert!(parse(&["ask", "what?", "--clarify", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn follows_names_the_rejected_question_a_new_one_asks_again() {
        let Command::Ask(args) = parse(&["ask", "what is done in billing?", "--follows", QUESTION])
            .unwrap()
            .command
        else {
            panic!("not ask");
        };
        assert_eq!(args.follows.as_ref().map(QuestionId::as_str), Some(QUESTION));
        assert!(parse(&["ask", "what?", "--follows", "q_1"]).is_err());
        assert!(parse(&["ask", "--wait", QUESTION, "--follows", QUESTION]).is_err());
        let help = parse(&["ask", "--help"]).unwrap_err().to_string();
        assert!(help.contains("--follows <QUESTION_ID>"), "{help}");
    }

    #[test]
    fn the_help_names_the_exit_codes_and_the_variables() {
        let help = Cli::command().render_long_help().to_string();
        for needle in [
            "0  answered",
            "2  refused",
            "3  a clarifying",
            "4  waiting",
            "MERCURY_URL",
            "MERCURY_TOKEN",
            "MERCURY_ASK_DEADLINE",
        ] {
            assert!(help.contains(needle), "{needle} missing from:\n{help}");
        }
    }
}
