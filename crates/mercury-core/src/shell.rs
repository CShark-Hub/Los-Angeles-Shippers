//! `mercury-core admin shell`: a prompt of its own for the admin commands. An admin opens it once,
//! on the host that runs the core (`docker compose exec core mercury-core admin shell`, or the same
//! over SSH with a terminal), and types the commands without what stands before them every time:
//! `user add …`, `access list`, `token create …`.
//!
//! A line is read as the arguments of `mercury-core admin`, by the same parser, so every command
//! has the `--help` and the checks it has outside the shell, and runs through the same code. A
//! command that fails says why and the prompt stays; the database is opened once for the whole
//! session. The shell asks for no login, as the commands don't: reaching the host that runs the
//! core is the access, and each change is an audit row of its own, written by the command. The
//! shell itself writes none, since it decides nothing.
//!
//! In a terminal the shell greets with a header (see [`crate::welcome`]) and reads lines with an
//! editor of its own (`line_editor`, `terminal`): the arrows walk the history, Tab completes, the
//! command is coloured as it is typed. Piped into, it is a script: no header, no prompt, the
//! commands' output alone, and it stops at its first line that fails.

use std::future::Future;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;

use clap::{CommandFactory, FromArgMatches};

use crate::admin;
use crate::cli::{Admin, ConfigCommand};
use crate::config;
use crate::db::{self, Db};
use crate::line_editor::{Node, Prompt};
use crate::welcome::{self, Header, Look};

/// What the prompt shows while it waits for a command, without colours.
pub const PROMPT: &str = "mercury> ";

/// How the shell talks to whoever is at it.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// A person at a terminal: the header, the prompt.
    pub interactive: bool,
    /// Colours in what the shell says itself.
    pub colour: bool,
    /// Box-drawing and block glyphs, or plain ASCII.
    pub unicode: bool,
}

impl Style {
    /// A script: nothing but what the commands say.
    pub const SCRIPT: Style = Style {
        interactive: false,
        colour: false,
        unicode: true,
    };

    pub fn look(&self) -> Look {
        Look {
            colour: self.colour,
            unicode: self.unicode,
        }
    }

    pub fn prompt(&self) -> Prompt {
        if self.colour {
            Prompt {
                styled: "\x1b[1;38;5;105mmercury\x1b[0m\x1b[90m>\x1b[0m ".to_owned(),
                width: PROMPT.chars().count(),
            }
        } else {
            Prompt::plain(PROMPT)
        }
    }

    fn error_label(&self) -> String {
        if self.colour {
            "\x1b[1;31merror:\x1b[0m".to_owned()
        } else {
            "error:".to_owned()
        }
    }
}

/// Where the shell's lines come from, and where its header goes.
pub trait Lines {
    /// The next line, or `None` when the input is over.
    fn read(&mut self, prompt: &Prompt, out: &mut dyn Write) -> io::Result<Option<String>>;

    /// Whether a header given now would be shown: it is gathered only then.
    fn wants_header(&self) -> bool {
        false
    }

    /// Shows the header; offered at the start and again after each command, while it is wanted.
    fn header(&mut self, _header: &Header, _out: &mut dyn Write) -> io::Result<()> {
        Ok(())
    }

    /// Done when the person asks to stop the command that is running (Ctrl-C at a terminal); never,
    /// where nobody can ask.
    fn interrupted(&mut self) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(std::future::pending())
    }

    /// Leaves the screen as it was found, when the session ends.
    fn finish(&mut self, _out: &mut dyn Write) -> io::Result<()> {
        Ok(())
    }
}

/// Lines read from a stream, one at a time: a script piped in, or a terminal where the editor isn't
/// available. At a terminal it shows its prompt and the header once, at the start.
pub struct PlainLines<R> {
    input: R,
    style: Style,
    shown: bool,
    width: usize,
}

impl<R: BufRead> PlainLines<R> {
    pub fn new(input: R, style: Style) -> Self {
        // no way to ask a terminal its size here: the frame at a width any terminal has
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|columns| columns.parse().ok())
            .unwrap_or(80);
        PlainLines {
            input,
            style,
            shown: false,
            width,
        }
    }
}

impl<R: BufRead> Lines for PlainLines<R> {
    fn wants_header(&self) -> bool {
        self.style.interactive && !self.shown
    }

    fn read(&mut self, prompt: &Prompt, out: &mut dyn Write) -> io::Result<Option<String>> {
        if self.style.interactive {
            write!(out, "{}", prompt.styled)?;
            out.flush()?;
        }
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            if self.style.interactive {
                // the prompt's line is left, not the next one's
                writeln!(out)?;
            }
            return Ok(None);
        }
        Ok(Some(line))
    }

    fn header(&mut self, header: &Header, out: &mut dyn Write) -> io::Result<()> {
        if self.shown || !self.style.interactive {
            return Ok(());
        }
        self.shown = true;
        for line in welcome::render(header, self.width, self.style.look()) {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }
}

/// One line typed at the prompt: the arguments of `mercury-core admin`.
#[derive(Debug, clap::Parser)]
#[command(
    // no program name before the command, so the usage lines read as what is typed here
    no_binary_name = true,
    name = "mercury",
    about = "The admin commands, typed here with nothing before them.",
    after_help = "Besides these: `help <command>` explains one, `exit` (or `quit`, or Ctrl-D) leaves."
)]
struct Line {
    #[command(subcommand)]
    command: Admin,
}

/// The commands as the prompt knows them. The shell's own command is left out of the lists it
/// prints: it is no use inside itself.
fn shell_command() -> clap::Command {
    Line::command().mut_subcommand("shell", |shell| shell.hide(true))
}

/// The same commands as a tree, for completion and colouring.
pub fn command_tree() -> &'static Node {
    static TREE: OnceLock<Node> = OnceLock::new();
    TREE.get_or_init(|| {
        let mut command = shell_command();
        // builds what clap adds itself: the `help` command, `--help`
        command.build();
        Node::from_clap(&command)
    })
}

/// Why a line couldn't be read as words.
#[derive(Debug, PartialEq, Eq)]
pub enum SplitError {
    /// A quote that is never closed, the one it opens.
    UnclosedQuote(char),
    /// A backslash with nothing after it.
    DanglingBackslash,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::UnclosedQuote(quote) => write!(f, "a {quote} is opened and never closed"),
            SplitError::DanglingBackslash => f.write_str("a \\ at the end of the line has nothing to escape"),
        }
    }
}

/// Splits a line into words as a shell would, for what an admin types: words are separated by
/// spaces, `'…'` keeps everything inside as it is, `"…"` keeps it too but reads `\"` and `\\`, and a
/// `\` outside quotes keeps the next character as part of the word. Nothing is expanded: no
/// variables, no globs, no commands.
pub fn split_line(line: &str) -> Result<Vec<String>, SplitError> {
    let mut words = Vec::new();
    let mut word = String::new();
    // whether a word has begun, so that `""` is an empty word and not nothing
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => word.push(inner),
                        None => return Err(SplitError::UnclosedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => word.push(escaped),
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => return Err(SplitError::UnclosedQuote('"')),
                        },
                        Some(inner) => word.push(inner),
                        None => return Err(SplitError::UnclosedQuote('"')),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(escaped) => word.push(escaped),
                    None => return Err(SplitError::DanglingBackslash),
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// How a session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// `exit`, `quit` or `\q`.
    Left,
    /// The input ran out (Ctrl-D, or the end of a script piped in).
    InputEnded,
    /// A script piped in stopped at this line, counted from 1, whose command failed: nothing after
    /// it ran.
    ScriptFailed(usize),
}

/// Whether a line did what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Done {
    Ok,
    Failed,
}

/// Reads commands from `lines` until the person leaves, and runs each one against `db` with
/// `config_dir` as the commands that name a project read it. What a command shows goes to `out`;
/// its notes and the shell's own errors go to `note`. At a terminal the session opens with the
/// header, and the header is drawn again after each command while `lines` wants it; a script piped
/// in gets neither it nor a prompt, so its output is the commands' alone.
///
/// At a terminal (`style.interactive`) a command that fails, or a line that doesn't read, ends
/// nothing: the error is said and the next line is read, and Ctrl-C while a command runs stops that
/// command. A script stops at its first line that fails, as `sh -e` would, so what came after a
/// failed change never runs on a wrong assumption. Only a failure to write to `out` or `note`, or to
/// read the input, is returned as an error.
pub async fn run(
    db: &Db,
    config_dir: &Path,
    database: &str,
    lines: &mut dyn Lines,
    out: &mut dyn Write,
    note: &mut dyn Write,
    style: &Style,
) -> io::Result<Ended> {
    let prompt = style.prompt();
    if lines.wants_header() {
        lines.header(&welcome::gather(db, config_dir, database, db::now()).await, out)?;
    }
    let mut number = 0;
    let ended = loop {
        let Some(line) = lines.read(&prompt, out)? else {
            break Ended::InputEnded;
        };
        number += 1;
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if matches!(text, "exit" | "quit" | "\\q") {
            break Ended::Left;
        }
        let done = tokio::select! {
            done = one_line(text, db, config_dir, out, note, style) => done?,
            () = lines.interrupted() => {
                // after the `^C` the terminal echoed
                writeln!(note)?;
                writeln!(
                    note,
                    "{} the command was stopped (Ctrl-C); a change it was making is in the audit log if it was made",
                    style.error_label()
                )?;
                Done::Failed
            }
        };
        out.flush()?;
        note.flush()?;
        if done == Done::Failed && !style.interactive {
            break Ended::ScriptFailed(number);
        }
        if lines.wants_header() {
            // a command may have changed what the header says
            lines.header(&welcome::gather(db, config_dir, database, db::now()).await, out)?;
        }
    };
    lines.finish(out)?;
    Ok(ended)
}

/// The words of a line as an admin command, by the parser the commands have outside the shell.
fn read_line(words: &[String]) -> Result<Admin, clap::Error> {
    let mut command = shell_command();
    let matches = command.try_get_matches_from_mut(words)?;
    Ok(Line::from_arg_matches(&matches)?.command)
}

/// What clap says, with its colours when the shell has them.
fn said(error: &clap::Error, style: &Style) -> String {
    if style.colour {
        error.render().ansi().to_string()
    } else {
        error.render().to_string()
    }
}

/// One line: read it as a command and run it. Everything that goes wrong is said at `note`, and the
/// line counts as failed.
async fn one_line(
    text: &str,
    db: &Db,
    config_dir: &Path,
    out: &mut dyn Write,
    note: &mut dyn Write,
    style: &Style,
) -> io::Result<Done> {
    let words = match split_line(text) {
        Ok(words) => words,
        Err(err) => {
            writeln!(note, "{} {err}", style.error_label())?;
            return Ok(Done::Failed);
        }
    };
    let command = match read_line(&words) {
        Ok(command) => command,
        // `help`, `<command> --help` and `--version` are answers, not failures
        Err(err) if !err.use_stderr() => {
            write!(out, "{}", said(&err, style))?;
            return Ok(Done::Ok);
        }
        Err(err) => {
            write!(note, "{}", said(&err, style))?;
            return Ok(Done::Failed);
        }
    };
    match command {
        Admin::Shell => {
            writeln!(note, "You are in the shell already; `exit` leaves it.")?;
            Ok(Done::Failed)
        }
        Admin::Config(ConfigCommand::Check { dir }) => {
            // the files only, as outside the shell: no database
            let dir = dir.unwrap_or_else(|| config_dir.to_owned());
            let report = config::read(&dir);
            config::render(&report, &dir, out)?;
            match report.problems.len() {
                0 => Ok(Done::Ok),
                n => {
                    writeln!(note, "{} config check: {n} problem(s)", style.error_label())?;
                    Ok(Done::Failed)
                }
            }
        }
        command => match admin::run(command, db, config_dir, out, note).await {
            Ok(()) => Ok(Done::Ok),
            Err(err) => {
                writeln!(note, "{} {err}", style.error_label())?;
                Ok(Done::Failed)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing;

    fn words(line: &str) -> Vec<String> {
        split_line(line).unwrap()
    }

    #[test]
    fn a_line_splits_at_spaces() {
        assert_eq!(
            words("user add --email a@example.com"),
            ["user", "add", "--email", "a@example.com"]
        );
        assert_eq!(words("  access   list  "), ["access", "list"]);
        assert!(words("").is_empty());
        assert!(words("   ").is_empty());
    }

    #[test]
    fn quotes_keep_a_name_with_spaces_in_one_word() {
        assert_eq!(words(r#"--name "Ann Lee""#), ["--name", "Ann Lee"]);
        assert_eq!(words("--name 'Ann Lee'"), ["--name", "Ann Lee"]);
        // a quote inside a word joins what is around it
        assert_eq!(words(r#"--name=An" n"Lee"#), ["--name=An nLee"]);
        // an empty pair of quotes is an empty word
        assert_eq!(words(r#"--label """#), ["--label", ""]);
    }

    #[test]
    fn a_backslash_escapes_one_character() {
        assert_eq!(words(r"Ann\ Lee"), ["Ann Lee"]);
        assert_eq!(words(r#""say \"hi\" \\ done""#), [r#"say "hi" \ done"#]);
        // inside single quotes nothing is special
        assert_eq!(words(r"'a\b'"), [r"a\b"]);
        // inside double quotes another character keeps its backslash
        assert_eq!(words(r#""a\nb""#), [r"a\nb"]);
    }

    #[test]
    fn nothing_is_expanded() {
        assert_eq!(words("$HOME `id` *"), ["$HOME", "`id`", "*"]);
    }

    #[test]
    fn a_quote_never_closed_and_a_backslash_at_the_end_are_said() {
        assert_eq!(split_line("--name 'Ann"), Err(SplitError::UnclosedQuote('\'')));
        assert_eq!(split_line("--name \"Ann"), Err(SplitError::UnclosedQuote('"')));
        assert_eq!(split_line("Ann\\"), Err(SplitError::DanglingBackslash));
        assert!(
            split_line("--name 'Ann")
                .unwrap_err()
                .to_string()
                .contains("never closed")
        );
    }

    /// Runs a script through the shell against a fresh database; what it said at `out` and at
    /// `note`, and how it ended.
    async fn session(script: &str, style: Style) -> (String, String, Ended, Db, tempfile::TempDir) {
        let (dir, db) = testing::db().await;
        let config = tempfile::tempdir().unwrap();
        let (out, note, ended) = session_on(&db, config.path(), script, style).await;
        (out, note, ended, db, dir)
    }

    async fn session_on(db: &Db, config_dir: &Path, script: &str, style: Style) -> (String, String, Ended) {
        let mut input = script.as_bytes();
        let (mut out, mut note) = (Vec::new(), Vec::new());
        let mut lines = PlainLines::new(&mut input, style);
        let ended = run(
            db,
            config_dir,
            "/data/mercury.db",
            &mut lines,
            &mut out,
            &mut note,
            &style,
        )
        .await
        .unwrap();
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(note).unwrap(),
            ended,
        )
    }

    const TERMINAL: Style = Style {
        interactive: true,
        colour: false,
        unicode: true,
    };

    #[tokio::test]
    async fn commands_run_as_outside_the_shell_without_what_stands_before_them() {
        let (out, note, ended, db, _dir) = session(
            "user add --email ann@example.com --name \"Ann Lee\" --role pm\naccess list\nexit\n",
            Style::SCRIPT,
        )
        .await;
        assert_eq!(ended, Ended::Left);
        assert!(note.is_empty(), "{note}");
        assert!(out.contains("added ann@example.com as pm"), "{out}");
        // the quoted name was one word, and the list shows the person just added
        assert!(out.contains("Ann Lee"), "{out}");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE type = 'user_added'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1, "the command writes its own audit row");
    }

    #[tokio::test]
    async fn at_a_terminal_a_failing_command_says_why_and_the_next_line_still_runs() {
        let (out, note, ended, _db, _dir) = session(
            "user add --email ann@example.com --name Ann --role pm\n\
             user add --email ann@example.com --name Ann --role pm\n\
             user add --email\n\
             nonsense\n\
             user add --name 'Open\n\
             access list\n",
            TERMINAL,
        )
        .await;
        assert_eq!(ended, Ended::InputEnded);
        // the second add is refused as a duplicate, by the same code as outside the shell
        assert!(note.contains("error: "), "{note}");
        // a missing value, an unknown command and an unclosed quote are each said
        assert!(note.contains("--email"), "{note}");
        assert!(note.contains("unrecognized subcommand 'nonsense'"), "{note}");
        assert!(note.contains("never closed"), "{note}");
        // and the last line was still read
        assert!(out.contains("ann@example.com"), "{out}");
    }

    #[tokio::test]
    async fn a_script_stops_at_its_first_failing_line_and_says_which() {
        let (out, note, ended, db, _dir) = session(
            "user add --email a@example.com --name A --role pm\n\
             # the same person again\n\
             user add --email a@example.com --name A --role pm\n\
             user add --email b@example.com --name B --role pm\n",
            Style::SCRIPT,
        )
        .await;
        // the line is counted with the comment before it, as an editor shows the file
        assert_eq!(ended, Ended::ScriptFailed(3));
        assert!(note.contains("error: "), "{note}");
        assert!(!out.contains("b@example.com"), "{out}");
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(users, 1, "nothing after the failed line ran");
        // a line that doesn't read stops it too
        let (_out, _note, ended, _db, _dir) = session("nonsense\naccess list\n", Style::SCRIPT).await;
        assert_eq!(ended, Ended::ScriptFailed(1));
    }

    #[tokio::test]
    async fn ctrl_c_while_a_command_runs_stops_it_and_the_prompt_stays() {
        // a terminal where Ctrl-C is pressed as soon as each command begins
        struct Impatient(std::collections::VecDeque<String>);
        impl Lines for Impatient {
            fn read(&mut self, _: &Prompt, _: &mut dyn Write) -> io::Result<Option<String>> {
                Ok(self.0.pop_front())
            }
            fn interrupted(&mut self) -> Pin<Box<dyn Future<Output = ()> + '_>> {
                Box::pin(std::future::ready(()))
            }
        }
        let (_dir, db) = testing::db().await;
        let config = tempfile::tempdir().unwrap();
        let mut lines = Impatient(["access list".to_owned(), "exit".to_owned()].into());
        let (mut out, mut note) = (Vec::new(), Vec::new());
        let ended = run(
            &db,
            config.path(),
            "/data/mercury.db",
            &mut lines,
            &mut out,
            &mut note,
            &TERMINAL,
        )
        .await
        .unwrap();
        let note = String::from_utf8(note).unwrap();
        assert!(note.contains("stopped (Ctrl-C)"), "{note}");
        // the next line was still read
        assert_eq!(ended, Ended::Left);
    }

    #[tokio::test]
    async fn help_and_a_commands_help_are_answers_not_errors() {
        let (out, note, ended, _db, _dir) = session("help\nuser add --help\n", Style::SCRIPT).await;
        // a script goes on after them
        assert_eq!(ended, Ended::InputEnded);
        assert!(note.is_empty(), "{note}");
        assert!(
            out.contains("user") && out.contains("token") && out.contains("access"),
            "{out}"
        );
        assert!(out.contains("exit"), "the shell's own words are listed: {out}");
        // its own command is no use inside it, so the list leaves it out
        assert!(!out.contains("  shell "), "{out}");
        assert!(out.contains("--email"), "{out}");
        // the usage reads as what is typed here, not as the program's command line
        assert!(!out.contains("mercury-core"), "{out}");
    }

    #[tokio::test]
    async fn exit_stops_the_session_and_nothing_after_it_runs() {
        let (out, _note, ended, db, _dir) = session(
            "user add --email a@example.com --name A --role pm\nquit\nuser add --email b@example.com --name B --role pm\n",
            Style::SCRIPT,
        )
        .await;
        assert_eq!(ended, Ended::Left);
        assert!(out.contains("a@example.com"));
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(users, 1);
    }

    #[tokio::test]
    async fn blank_lines_and_comments_are_skipped() {
        let (out, note, ended, _db, _dir) = session(
            "\n   \n# a note to myself\nuser add --email a@example.com --name A --role pm\n\n  # another\n",
            Style::SCRIPT,
        )
        .await;
        assert_eq!(ended, Ended::InputEnded);
        assert!(note.is_empty(), "{note}");
        assert!(out.contains("added a@example.com"), "{out}");
    }

    #[tokio::test]
    async fn the_shell_cannot_open_a_shell_in_itself() {
        let (out, note, _ended, _db, _dir) = session("shell\n", Style::SCRIPT).await;
        assert!(out.is_empty(), "{out}");
        assert!(note.contains("already"), "{note}");
    }

    #[tokio::test]
    async fn config_check_runs_without_the_database_and_names_a_problem() {
        let (dir, db) = testing::db().await;
        // closed: a command that touched the database would fail with a database error
        db.pool().close().await;
        let empty = tempfile::tempdir().unwrap();
        let (out, note, ended) = session_on(&db, empty.path(), "config check\n", Style::SCRIPT).await;
        // an empty directory has none of the files: the problems are listed and counted
        assert!(note.contains("config check:"), "{out} {note}");
        assert!(!note.contains("closed") && !note.contains("database"), "{note}");
        assert_eq!(
            ended,
            Ended::ScriptFailed(1),
            "problems are a failure, as outside the shell"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn at_a_terminal_it_greets_with_the_header_and_prompts_before_each_line() {
        let (out, note, ended, _db, _dir) = session("access list\n", TERMINAL).await;
        assert_eq!(ended, Ended::InputEnded);
        assert!(note.is_empty(), "{note}");
        assert!(out.contains("Mercury admin"), "{out}");
        assert!(out.contains("0 people"), "{out}");
        assert!(out.contains("no probe has registered"), "{out}");
        // a prompt before each line read, the last one answered by the end of the input
        assert_eq!(out.matches(PROMPT).count(), 2, "{out}");
        assert!(out.ends_with('\n'), "{out:?}");
        // the header is shown once, not after every command, by a source that can't redraw
        assert_eq!(out.matches('╭').count(), 1, "{out}");
    }

    #[tokio::test]
    async fn the_header_counts_the_people_just_added() {
        // a source that draws the header each time it is given one
        struct Headers(Vec<String>, std::collections::VecDeque<String>);
        impl Lines for Headers {
            fn read(&mut self, _: &Prompt, _: &mut dyn Write) -> io::Result<Option<String>> {
                Ok(self.1.pop_front())
            }
            fn wants_header(&self) -> bool {
                true
            }
            fn header(&mut self, header: &Header, _: &mut dyn Write) -> io::Result<()> {
                self.0
                    .push(welcome::render(header, 90, Style::SCRIPT.look()).join("\n"));
                Ok(())
            }
        }
        let (_dir, db) = testing::db().await;
        let config = tempfile::tempdir().unwrap();
        let mut lines = Headers(
            Vec::new(),
            [
                "user add --email a@example.com --name A --role pm".to_owned(),
                "help".to_owned(),
            ]
            .into(),
        );
        let (mut out, mut note) = (Vec::new(), Vec::new());
        run(
            &db,
            config.path(),
            "/data/mercury.db",
            &mut lines,
            &mut out,
            &mut note,
            &TERMINAL,
        )
        .await
        .unwrap();
        // once at the start, once after each command
        assert_eq!(lines.0.len(), 3);
        assert!(lines.0[0].contains("0 people"), "{}", lines.0[0]);
        assert!(lines.0[1].contains("1 person"), "{}", lines.0[1]);
        // gathering it three times wrote nothing: the one audit row is the command's
        let rows: Vec<String> = sqlx::query_scalar("SELECT type FROM audit_events")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, ["user_added"]);
    }

    #[tokio::test]
    async fn piped_in_it_prints_no_header_and_no_prompt() {
        let (out, _note, _ended, _db, _dir) = session("access list\n", Style::SCRIPT).await;
        assert!(!out.contains(PROMPT) && !out.contains("Mercury admin"), "{out}");
    }

    #[tokio::test]
    async fn errors_and_help_are_coloured_only_when_the_shell_has_colours() {
        let coloured = Style {
            colour: true,
            ..Style::SCRIPT
        };
        let (_out, note, _ended, _db, _dir) = session("nonsense\n", coloured).await;
        assert!(note.contains("\x1b["), "{note:?}");
        let (_out, plain, _ended, _db, _dir) = session("nonsense\n", Style::SCRIPT).await;
        assert!(!plain.contains('\x1b'), "{plain:?}");
        let (shown, _note, _ended, _db, _dir) = session("user add --email\n", coloured).await;
        assert!(shown.is_empty());
    }

    #[test]
    fn the_prompt_is_as_wide_with_colours_as_without() {
        let coloured = Style {
            colour: true,
            ..Style::SCRIPT
        };
        assert_eq!(coloured.prompt().width, Style::SCRIPT.prompt().width);
        assert!(coloured.prompt().styled.contains("\x1b["));
        assert_eq!(Style::SCRIPT.prompt().styled, PROMPT);
    }

    #[test]
    fn the_command_tree_knows_the_commands_flags_and_hides_the_shell() {
        let tree = command_tree();
        let user = tree.subs.iter().find(|n| n.name == "user").unwrap();
        let add = user.subs.iter().find(|n| n.name == "add").unwrap();
        for flag in ["--email", "--name", "--role", "--help"] {
            assert!(add.flags.iter().any(|f| f == flag), "{flag}: {:?}", add.flags);
        }
        assert!(tree.subs.iter().any(|n| n.name == "shell" && n.hidden));
        assert!(tree.subs.iter().any(|n| n.name == "help"));
    }
}
