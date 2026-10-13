//! The commands: each finds the sign-in, talks to the server, prints, and gives its exit code.
//! The keychain is used outside the async runtime, since a keychain call blocks.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::SecondsFormat;

use mercury_schema::api::{ApprovalDecision, Decision, ErrorBody, ErrorDetail};

use crate::ask::{self, Question};
use crate::cli::{ApprovalsArgs, AskArgs, AuditArgs, Command};
use crate::client::Client;
use crate::credentials::{self, Login, Server, Store, TOKEN_VAR};
use crate::error::CliError;
use crate::render::{self, Timeline};

/// The exit code of any error.
pub const ERROR: u8 = 1;

/// The variable that sets how long asking waits for the question's end.
const DEADLINE_VAR: &str = "MERCURY_ASK_DEADLINE";

/// What a command reads and writes; the binary passes the process's own.
pub struct Io<'a> {
    pub var: &'a dyn Fn(&str) -> Option<String>,
    pub store: &'a dyn Store,
    pub stdin: &'a mut dyn BufRead,
    /// Whether standard input is a terminal, where `login` asks for the token.
    pub stdin_is_terminal: bool,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

/// Runs a command and gives its exit code. Errors go to standard error; with `--json`, an error
/// the server answered also goes to standard output, as its error body.
pub fn run(command: Command, mut io: Io<'_>) -> u8 {
    let json = command.json();
    let result = dispatch(command, &mut io);
    let _ = io.stdout.flush();
    match result {
        Ok(code) => code,
        Err(err) => {
            if json
                && let CliError::Api {
                    code, message, hint, ..
                } = &err
            {
                let body = ErrorBody {
                    error: ErrorDetail {
                        code: *code,
                        message: message.clone(),
                        hint: hint.clone(),
                    },
                };
                let _ = render::json(&body, io.stdout);
                let _ = io.stdout.flush();
            }
            report(&err, io.stderr);
            ERROR
        }
    }
}

fn report(err: &CliError, stderr: &mut dyn Write) {
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let _ = writeln!(stderr, "mercury: {}", clean(&err.to_string()));
    if let Some(hint) = err.hint() {
        let _ = writeln!(stderr, "  {}", clean(&hint));
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::Output)
}

fn dispatch(command: Command, io: &mut Io<'_>) -> Result<u8, CliError> {
    match command {
        Command::Login { url, token } => login(&url, token, io),
        Command::Logout => {
            if io.store.clear()? {
                writeln!(io.stdout, "Signed out: the token is gone from the OS keychain.")?;
            } else {
                writeln!(io.stdout, "Not signed in: there was nothing to remove.")?;
            }
            Ok(0)
        }
        Command::Me { json } => {
            let client = Client::new(&credentials::resolve(io.var, io.store)?)?;
            let me = runtime()?.block_on(client.me())?;
            if json {
                render::json(&me, io.stdout)?;
            } else {
                render::me(&me, client.server(), io.stdout)?;
            }
            Ok(0)
        }
        Command::Projects { json } => {
            let client = Client::new(&credentials::resolve(io.var, io.store)?)?;
            let list = runtime()?.block_on(client.projects())?;
            if json {
                render::json(&list, io.stdout)?;
            } else {
                render::projects(&list, io.stdout)?;
            }
            Ok(0)
        }
        Command::Ask(args) => ask(args, io),
        Command::Audit(args) => audit(args, io),
        Command::Approvals(args) => approvals(args, io),
    }
}

/// `mercury approvals`: the answers waiting for the caller's sign-off, or one decision on one of them.
fn approvals(args: ApprovalsArgs, io: &mut Io<'_>) -> Result<u8, CliError> {
    let client = Client::new(&credentials::resolve(io.var, io.store)?)?;
    let runtime = runtime()?;
    let decision = match (&args.approve, &args.reject) {
        (Some(id), _) => Some((id, Decision::Approve)),
        (None, Some(id)) => Some((id, Decision::Reject)),
        (None, None) => None,
    };
    if let Some((id, decision)) = decision {
        let body = ApprovalDecision {
            decision,
            note: args.note.clone(),
        };
        runtime.block_on(client.decide(id, &body))?;
        render::decided(decision, io.stdout)?;
        return Ok(0);
    }
    let list = runtime.block_on(client.approvals())?;
    if args.json {
        render::approvals_json(&list, io.stdout)?;
    } else {
        render::approvals(&list, io.stdout)?;
    }
    Ok(0)
}

/// The most rows `mercury audit` prints; past them it says how to narrow the list.
const AUDIT_ROWS: usize = 2000;

/// `mercury audit`: the audit rows, or with `--export` every matching row into a file. Both are an
/// admin's alone; anyone else gets the server's refusal, in its words, and exit code 1.
fn audit(args: AuditArgs, io: &mut Io<'_>) -> Result<u8, CliError> {
    let client = Client::new(&credentials::resolve(io.var, io.store)?)?;
    let mut filters = Vec::new();
    if let Some(project) = &args.project {
        filters.push(("project", project.as_str().to_owned()));
    }
    if let Some(since) = args.since {
        filters.push(("from", since.to_rfc3339_opts(SecondsFormat::Secs, true)));
    }
    let runtime = runtime()?;
    if let Some(path) = &args.export {
        let rows = runtime.block_on(export_to(&client, &filters, path))?;
        writeln!(io.stdout, "Exported {rows} row(s) to {}.", path.display())?;
        return Ok(0);
    }
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let more = loop {
        let page = runtime.block_on(client.audit(&filters, cursor.as_deref()))?;
        items.extend(page.items);
        match page.next {
            Some(next) if items.len() < AUDIT_ROWS => cursor = Some(next),
            Some(_) => break true,
            None => break false,
        }
    };
    items.truncate(AUDIT_ROWS);
    render::audit(&items, more, io.stdout)?;
    Ok(0)
}

/// Writes the export into a new file beside `path` and moves it over `path` once it is whole. A
/// refused or broken export leaves no half file and, above all, leaves what was at `path` as it
/// was: the file is never truncated first, and only the file this export made is ever deleted.
pub async fn export_to(client: &Client, filters: &[(&str, String)], path: &Path) -> Result<u64, CliError> {
    let part = part_beside(path)?;
    // `create_new` never opens a file somebody else made
    let mut file = OpenOptions::new().write(true).create_new(true).open(&part)?;
    let written = client.audit_export(filters, &mut file).await;
    let whole = written.and_then(|rows| file.sync_all().map(|()| rows).map_err(CliError::from));
    drop(file);
    let rows = match whole {
        Ok(rows) => rows,
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
    };
    if let Err(err) = std::fs::rename(&part, path) {
        let _ = std::fs::remove_file(&part);
        return Err(err.into());
    }
    Ok(rows)
}

/// A name for the file an export is written to before it replaces `path`: in the same directory,
/// so the move is a rename, and unlikely to be anybody's.
fn part_beside(path: &Path) -> Result<PathBuf, CliError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CliError::Usage("--export needs the name of a file".to_owned()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    Ok(path.with_file_name(format!(".{name}.{}-{nanos}.part", std::process::id())))
}

/// Checks the token with `GET /v1/me` before keeping it, so a wrong one is never stored.
fn login(url: &str, token: Option<String>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let server = Server::parse(url)?;
    let raw = match token {
        Some(raw) => raw,
        None => {
            if io.stdin_is_terminal {
                write!(io.stderr, "Paste your token, then press Enter: ")?;
                io.stderr.flush()?;
            }
            let mut line = String::new();
            io.stdin.read_line(&mut line)?;
            line
        }
    };
    let login = Login {
        server,
        token: credentials::token(&raw)?,
    };
    let me = runtime()?.block_on(Client::new(&login)?.me())?;
    io.store.save(&login)?;
    render::signed_in(&me, &login.server, io.stdout)?;
    if (io.var)(TOKEN_VAR).is_some_and(|value| !value.trim().is_empty()) {
        writeln!(
            io.stderr,
            "note: MERCURY_TOKEN is set, so commands use it instead of this sign-in"
        )?;
    }
    Ok(0)
}

/// How long asking waits for the question's end: `MERCURY_ASK_DEADLINE` in whole seconds, from 1 to
/// a day, and two minutes when it is unset or empty.
fn ask_deadline(var: &dyn Fn(&str) -> Option<String>) -> Result<Duration, CliError> {
    let Some(raw) = var(DEADLINE_VAR).filter(|raw| !raw.trim().is_empty()) else {
        return Ok(ask::DEFAULT_DEADLINE);
    };
    let wrong = || {
        CliError::Usage(format!(
            "{DEADLINE_VAR} must be a whole number of seconds, from 1 to {}",
            ask::MAX_DEADLINE.as_secs()
        ))
    };
    let seconds: u64 = raw.trim().parse().map_err(|_| wrong())?;
    let deadline = Duration::from_secs(seconds);
    if deadline.is_zero() || deadline > ask::MAX_DEADLINE {
        return Err(wrong());
    }
    Ok(deadline)
}

fn ask(args: AskArgs, io: &mut Io<'_>) -> Result<u8, CliError> {
    let var = io.var;
    let question = match (&args.wait, args.text) {
        (Some(_), _) => None,
        (None, Some(text)) if !text.trim().is_empty() => Some(Question {
            text,
            project: args.project,
            clarify: args.clarify,
            follows: args.follows,
        }),
        (None, _) => return Err(CliError::Usage("the question is empty".to_owned())),
    };
    // checked before any request: a wrong value is a mistake of the command line, not of the server
    let deadline = match question {
        Some(_) => Some(ask_deadline(var)?),
        None => None,
    };
    let client = Client::new(&credentials::resolve(io.var, io.store)?)?;
    let runtime = runtime()?;
    let ask::Followed {
        summary,
        deadline_passed,
    } = {
        let mut timeline = Timeline::new(&mut *io.stderr);
        match (question, deadline, args.wait) {
            (Some(question), Some(deadline), _) => {
                runtime.block_on(ask::ask(&client, question, deadline, &mut timeline))?
            }
            (None, _, Some(id)) => runtime.block_on(ask::wait(&client, id, &mut timeline))?,
            _ => return Err(CliError::Usage("the question is empty".to_owned())),
        }
    };
    if args.json {
        render::json(&summary, io.stdout)?;
    } else {
        render::result(&summary, io.stdout)?;
    }
    let outcome = summary.outcome();
    if outcome == ask::Outcome::Failed {
        io.stdout.flush()?;
        let err = match &summary.error {
            Some(error) => CliError::Failed {
                code: error.code,
                message: error.message.clone(),
            },
            // a `done` came, the answer didn't
            None if summary.audit_id.is_some() => CliError::NoAnswer,
            // the question hadn't ended when the deadline passed: it may still be running
            None if deadline_passed.is_some() => CliError::Deadline {
                id: summary.id.clone(),
                seconds: deadline_passed.map_or(0, |limit| limit.as_secs()),
            },
            None => CliError::Lost(summary.id.clone()),
        };
        report(&err, io.stderr);
    }
    Ok(outcome.exit_code())
}
