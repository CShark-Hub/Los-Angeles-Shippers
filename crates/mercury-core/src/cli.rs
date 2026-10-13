//! The `mercury-core` command line: the server when no command is given, the container's health
//! check, and the admin commands. Every change an admin makes runs here, on the host that runs the
//! core (`docker compose exec core mercury-core admin …`, or `… admin shell` for a prompt of its
//! own); the web client's admin panel only reads.

use std::path::PathBuf;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use mercury_schema::ids::TokenPublicId;
use mercury_schema::{ProjectId, QuestionId, Role};

#[derive(Debug, Parser)]
#[command(
    name = "mercury-core",
    version,
    about = "Mercury's gateway. Without a command it runs the server."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ask the running core's /v1/health, for the container's health check
    Healthcheck,
    /// People, grants and tokens, in the database at MERCURY_DB; and the configuration check
    #[command(subcommand)]
    Admin(Admin),
}

#[derive(Debug, Subcommand)]
pub enum Admin {
    /// Add people, change their role, grant and take back projects, disable them
    #[command(subcommand)]
    User(UserCommand),
    /// Create, list and revoke personal tokens
    #[command(subcommand)]
    Token(TokenCommand),
    /// See who may ask about what
    #[command(subcommand)]
    Access(AccessCommand),
    /// Check the configuration files, without starting anything
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Open a prompt of its own for these commands: type them without `mercury-core admin` before
    /// them (`user add …`, `access list`), one after another, with the database opened once.
    /// `help` lists them, `<command> --help` explains one, `exit` or Ctrl-D leaves. A command that
    /// fails says why and the prompt stays. It needs a terminal to be comfortable (`docker compose
    /// exec core …` has one; over SSH use `ssh -t`); lines piped in run as a script.
    Shell,
    /// See the probes; quarantine or release one
    #[command(subcommand)]
    Probe(ProbeCommand),
    /// Check the audit chain, or export its rows
    #[command(subcommand)]
    Audit(AuditCommand),
    /// What to look at daily: the alerts that hold, the probes and their state, the backend of each
    /// model role, today's spend (UTC) against the caps in models.yaml, and which judge answered
    /// the questions that ended in the last 24 hours. Reads the database, models.yaml and
    /// projects.yaml only; it asks no model and no probe.
    ///
    /// The three alerts: today's spend at 80% of the daily budget (at once when that budget is 0),
    /// the judge degraded for more than 5 minutes, a probe silent for more than 15 (or one of
    /// projects.yaml that never registered). The core logs each once an hour.
    Status {
        /// Say only the alerts that hold, one line each, and exit 1 if any does; a file it can't
        /// read is a line of its own and exits 1 too, since a check that can't look is no pass;
        /// with none, say and write nothing and exit 0. For cron, or a loop that runs every minute
        #[arg(long, conflicts_with = "since")]
        check: bool,
        /// Also count the questions asked since this time (RFC 3339, or a date for its midnight
        /// UTC) by channel (cli, web, mcp) and outcome: answered, refused, degraded, held
        #[arg(long, value_name = "TIME", value_parser = moment)]
        since: Option<DateTime<Utc>>,
    },
    /// Copy the whole database to a file, while the core keeps serving
    ///
    /// SQLite's VACUUM INTO: one consistent snapshot. The file holds the people and their grants,
    /// the tokens' and the browser sessions' digests, the questions with their redacted texts and
    /// answers, and the audit chain: keep it like a secret. An existing file is never overwritten.
    /// The backup is an audit row with the file's name and size.
    Backup {
        /// The file to create, e.g. /data/backups/mercury-2026-10-06.db; a missing folder is made
        #[arg(long, value_name = "FILE")]
        to: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProbeCommand {
    /// Every registered probe: its status, the access mode it reads with and what limited it,
    /// when it last registered, and why it is quarantined
    List,
    /// Stop calling a project's probe until it is released
    Quarantine {
        /// The project, as projects.yaml names it
        project: String,
        /// Why, for the next admin
        #[arg(long)]
        reason: String,
    },
    /// Call a quarantined probe again; it is checked at its next registration
    Release {
        /// The project, as projects.yaml names it
        project: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Add a person. An e-mail address belongs to one person only.
    Add {
        /// Their e-mail address, by which every other command finds them
        #[arg(long)]
        email: String,
        /// The name Mercury shows
        #[arg(long)]
        name: String,
        /// Their role; policies.yaml says what each role may ask
        #[arg(long, value_enum)]
        role: RoleArg,
    },
    /// Grant projects. Granting a project again sets its approver flag to what this command says.
    /// A project the projects.yaml in MERCURY_CONFIG_DIR doesn't list is refused, with the ones it
    /// lists, and nothing is granted.
    Grant {
        #[arg(long)]
        email: String,
        /// Project ids, comma-separated, as projects.yaml lists them; `portfolio` for answers
        /// across projects
        #[arg(long, value_delimiter = ',', required = true)]
        projects: Vec<String>,
        /// They also approve the answers on these projects that need a sign-off; only a project
        /// manager or a tech lead may (`user set-role` first)
        #[arg(long)]
        approver: bool,
    },
    /// Change a person's role. It holds from their next request: every request reads the role.
    /// Setting the role they have changes nothing and says so. A disabled person is refused, and so
    /// is board or admin for someone who approves a project (grant it again without --approver first).
    SetRole {
        #[arg(long)]
        email: String,
        /// The new role; policies.yaml says what each role may ask
        #[arg(long, value_enum)]
        role: RoleArg,
    },
    /// Take projects back, with their approver flags, and print the person's projects after. A
    /// project they weren't granted changes nothing and says so. A disabled person is refused.
    Ungrant {
        #[arg(long)]
        email: String,
        /// Project ids, comma-separated; `portfolio` for answers across projects
        #[arg(long, value_delimiter = ',', required = true)]
        projects: Vec<String>,
    },
    /// Disable a person and revoke every token they hold, in one step
    Disable {
        #[arg(long)]
        email: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Create a token and print it once. It can't be shown again.
    Create {
        /// Whose token it is
        #[arg(long)]
        email: String,
        /// What it is for, e.g. the device
        #[arg(long)]
        label: String,
        /// How long it is valid, in days (90d) or hours (12h)
        #[arg(long, default_value = "90d", value_parser = parse_validity)]
        expires: TimeDelta,
    },
    /// List tokens with their state. Never a secret: only `token create` shows one, once.
    List {
        /// Only this person's tokens
        #[arg(long)]
        email: Option<String>,
    },
    /// Revoke a token. Its next request is refused.
    Revoke {
        /// The token's public id, as `token list` shows it
        public_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Recompute the whole hash chain and name the first row that breaks it; exit 1 if one does
    Verify,
    /// Write the rows to standard output, one JSON object per line in seq order, with the hashes
    /// that verify them; redirect it to a file. The filters combine; without one, every row. The
    /// export is itself an audit row.
    Export {
        /// Everything this token did: its questions, sign-ins, refusals, creation and revocation
        #[arg(long, value_name = "PUBLIC_ID", value_parser = token_id)]
        token: Option<TokenPublicId>,
        /// One question's rows
        #[arg(long, value_name = "QUESTION_ID", value_parser = question_id)]
        question: Option<QuestionId>,
        /// The rows of one project
        #[arg(long, value_parser = project_id)]
        project: Option<ProjectId>,
        /// From this time on, included: RFC 3339, or a date for its midnight UTC
        #[arg(long, value_name = "TIME", value_parser = moment)]
        from: Option<DateTime<Utc>>,
        /// Before this time, not included: RFC 3339, or a date for its midnight UTC
        #[arg(long, value_name = "TIME", value_parser = moment)]
        to: Option<DateTime<Utc>>,
    },
}

fn token_id(raw: &str) -> Result<TokenPublicId, String> {
    TokenPublicId::parse(raw.trim())
        .map_err(|_| "a token's public id is 12 letters and digits, as `token list` shows it".to_owned())
}

fn question_id(raw: &str) -> Result<QuestionId, String> {
    QuestionId::parse(raw.trim()).map_err(|_| "a question id is q_ and 26 letters and digits".to_owned())
}

fn project_id(raw: &str) -> Result<ProjectId, String> {
    ProjectId::parse(raw.trim())
        .map_err(|_| "a project id is lowercase letters, digits and dashes, like delta".to_owned())
}

/// RFC 3339, or a date for its midnight UTC.
pub fn moment(raw: &str) -> Result<DateTime<Utc>, String> {
    let raw = raw.trim();
    crate::db::parse_time(raw)
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .ok()
                .and_then(|day| day.and_hms_opt(0, 0, 0))
                .map(|midnight| midnight.and_utc())
        })
        .ok_or_else(|| "a time is RFC 3339, like 2026-10-01T12:00:00Z, or a date, like 2026-10-01".to_owned())
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Read every file, and the links between them, as the core does at start. One line per
    /// problem, naming the file, the key and the position; exit 1 on any problem. Missing or
    /// empty secret files are notes.
    Check {
        /// The configuration directory; MERCURY_CONFIG_DIR, or /config, by default
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccessCommand {
    /// Every person with their role, projects and approver flags, what each grant lets them ask
    /// (the policy and the project's manifest combined), and every token with its state
    List {
        /// Only the people granted this project, with the project's probe
        #[arg(long)]
        project: Option<String>,
        /// Only this person
        #[arg(long)]
        email: Option<String>,
    },
}

/// A role as the command line names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum RoleArg {
    Pm,
    TechLead,
    Board,
    Admin,
}

impl From<RoleArg> for Role {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::Pm => Role::Pm,
            RoleArg::TechLead => Role::TechLead,
            RoleArg::Board => Role::Board,
            RoleArg::Admin => Role::Admin,
        }
    }
}

/// A validity like `90d` or `12h`: a whole number above zero, then `d` or `h`.
pub fn parse_validity(text: &str) -> Result<TimeDelta, String> {
    let wrong = || format!("{text:?} is not a validity like 90d or 12h");
    let unit = text.chars().last().ok_or_else(wrong)?;
    let count: u32 = text[..text.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| wrong())?;
    if count == 0 {
        return Err(wrong());
    }
    match unit {
        'd' => Ok(TimeDelta::days(i64::from(count))),
        'h' => Ok(TimeDelta::hours(i64::from(count))),
        _ => Err(wrong()),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mercury-core").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_command_runs_the_server_and_healthcheck_stays() {
        assert!(parse(&[]).unwrap().command.is_none());
        assert!(matches!(
            parse(&["healthcheck"]).unwrap().command,
            Some(Command::Healthcheck)
        ));
    }

    #[test]
    fn the_shell_opens_from_the_admin_command_and_takes_no_arguments() {
        assert!(matches!(
            parse(&["admin", "shell"]).unwrap().command,
            Some(Command::Admin(Admin::Shell))
        ));
        assert!(parse(&["admin", "shell", "extra"]).is_err());
        let help = parse(&["admin", "shell", "--help"]).unwrap_err().to_string();
        assert!(help.contains("exit"), "{help}");
        assert!(help.contains("Ctrl-D"), "{help}");
    }

    #[test]
    fn every_admin_command_has_help() {
        for command in [
            &["admin"][..],
            &["admin", "user"],
            &["admin", "user", "add"],
            &["admin", "user", "grant"],
            &["admin", "user", "set-role"],
            &["admin", "user", "ungrant"],
            &["admin", "user", "disable"],
            &["admin", "token"],
            &["admin", "token", "create"],
            &["admin", "token", "list"],
            &["admin", "token", "revoke"],
            &["admin", "access"],
            &["admin", "access", "list"],
            &["admin", "config"],
            &["admin", "config", "check"],
            &["admin", "probe"],
            &["admin", "probe", "list"],
            &["admin", "probe", "quarantine"],
            &["admin", "probe", "release"],
            &["admin", "audit"],
            &["admin", "audit", "verify"],
            &["admin", "audit", "export"],
            &["admin", "status"],
            &["admin", "backup"],
            &["admin", "shell"],
        ] {
            let args: Vec<&str> = command.iter().copied().chain(["--help"]).collect();
            let err = parse(&args).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::DisplayHelp, "{command:?}");
        }
    }

    #[test]
    fn the_help_of_audit_export_lists_its_filters() {
        let help = parse(&["admin", "audit", "export", "--help"])
            .unwrap_err()
            .to_string();
        for flag in ["--token", "--question", "--project", "--from", "--to"] {
            assert!(help.contains(flag), "{flag} in {help}");
        }
        assert!(parse(&["admin", "audit", "export", "--token", "short"]).is_err());
        assert!(parse(&["admin", "audit", "export", "--from", "yesterday"]).is_err());
        assert!(
            parse(&[
                "admin",
                "audit",
                "export",
                "--from",
                "2026-10-01",
                "--to",
                "2026-10-02T12:00:00Z"
            ])
            .is_ok()
        );
    }

    #[test]
    fn the_help_of_access_list_says_it_shows_what_each_grant_lets_a_person_ask() {
        let help = parse(&["admin", "access", "list", "--help"])
            .unwrap_err()
            .to_string();
        // single words: the help may wrap anywhere between them
        for word in [
            "role",
            "approver",
            "ask",
            "policy",
            "manifest",
            "token",
            "--project",
            "--email",
        ] {
            assert!(help.contains(word), "{word} in {help}");
        }
    }

    #[test]
    fn the_help_of_status_says_what_it_shows_and_what_it_reads() {
        let help = parse(&["admin", "status", "--help"]).unwrap_err().to_string();
        // single words: the help may wrap anywhere between them
        for word in [
            "alerts",
            "probes",
            "backend",
            "spend",
            "judge",
            "hours",
            "models.yaml",
            "projects.yaml",
            "asks",
            "--check",
            "--since",
            "exit",
            "cron",
            "pass",
            "channel",
            "outcome",
        ] {
            assert!(help.contains(word), "{word} in {help}");
        }
        assert!(matches!(
            parse(&["admin", "status"]).unwrap().command,
            Some(Command::Admin(Admin::Status {
                check: false,
                since: None
            }))
        ));
        assert_eq!(
            parse(&["admin", "status", "--all"]).unwrap_err().exit_code(),
            2,
            "it takes nothing else"
        );
    }

    #[test]
    fn status_takes_a_check_or_a_start_date_and_not_both() {
        assert!(matches!(
            parse(&["admin", "status", "--check"]).unwrap().command,
            Some(Command::Admin(Admin::Status {
                check: true,
                since: None
            }))
        ));
        let Some(Command::Admin(Admin::Status { check, since })) =
            parse(&["admin", "status", "--since", "2026-10-07"])
                .unwrap()
                .command
        else {
            panic!("not a status");
        };
        assert!(!check);
        assert_eq!(since, crate::db::parse_time("2026-10-07T00:00:00.000Z"));
        let Some(Command::Admin(Admin::Status { since, .. })) =
            parse(&["admin", "status", "--since", "2026-10-07T12:30:00Z"])
                .unwrap()
                .command
        else {
            panic!("not a status");
        };
        assert_eq!(since, crate::db::parse_time("2026-10-07T12:30:00.000Z"));
        for args in [
            &["admin", "status", "--check", "--since", "2026-10-07"][..],
            &["admin", "status", "--since", "yesterday"][..],
            &["admin", "status", "--since"][..],
        ] {
            assert_eq!(parse(args).unwrap_err().exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn the_help_of_backup_names_its_flag_and_what_the_file_holds() {
        let help = parse(&["admin", "backup", "--help"]).unwrap_err().to_string();
        // single words: the help may wrap anywhere between them
        for word in [
            "--to",
            "people",
            "tokens",
            "sessions",
            "questions",
            "audit",
            "overwritten",
            "secret",
        ] {
            assert!(help.contains(word), "{word} in {help}");
        }
        assert_eq!(
            parse(&["admin", "backup"]).unwrap_err().exit_code(),
            2,
            "a backup needs its file"
        );
    }

    #[test]
    fn the_help_of_user_add_names_the_roles() {
        let help = parse(&["admin", "user", "add", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("pm, tech_lead, board, admin"), "{help}");
    }

    #[test]
    fn the_help_of_user_grant_says_what_it_refuses() {
        let help = parse(&["admin", "user", "grant", "--help"])
            .unwrap_err()
            .to_string();
        assert!(
            help.contains("projects.yaml") && help.contains("MERCURY_CONFIG_DIR") && help.contains("refused"),
            "{help}"
        );
    }

    #[test]
    fn set_role_takes_one_of_the_four_roles_and_ungrant_a_list_of_projects() {
        let set = parse(&[
            "admin",
            "user",
            "set-role",
            "--email",
            "anna@example.com",
            "--role",
            "tech_lead",
        ])
        .unwrap();
        let Some(Command::Admin(Admin::User(UserCommand::SetRole { email, role }))) = set.command else {
            panic!("not a set-role");
        };
        assert_eq!((email.as_str(), role), ("anna@example.com", RoleArg::TechLead));
        let help = parse(&["admin", "user", "set-role", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("pm, tech_lead, board, admin"), "{help}");
        // single words: the help may wrap anywhere between them
        for word in ["next", "request", "nothing", "disabled"] {
            assert!(help.contains(word), "{word} in {help}");
        }

        let ungrant = parse(&[
            "admin",
            "user",
            "ungrant",
            "--email",
            "anna@example.com",
            "--projects",
            "delta,portfolio",
        ])
        .unwrap();
        let Some(Command::Admin(Admin::User(UserCommand::Ungrant { projects, .. }))) = ungrant.command else {
            panic!("not an ungrant");
        };
        assert_eq!(projects, ["delta", "portfolio"]);
        let help = parse(&["admin", "user", "ungrant", "--help"])
            .unwrap_err()
            .to_string();
        for word in ["approver", "nothing", "disabled", "portfolio"] {
            assert!(help.contains(word), "{word} in {help}");
        }

        for args in [
            &["admin", "user", "set-role", "--email", "anna@example.com"][..],
            &[
                "admin",
                "user",
                "set-role",
                "--email",
                "anna@example.com",
                "--role",
                "approver",
            ],
            &["admin", "user", "ungrant", "--email", "anna@example.com"],
            &["admin", "user", "ungrant", "--projects", "delta"],
        ] {
            assert_eq!(parse(args).unwrap_err().exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn the_deployment_guides_commands_parse() {
        let add = parse(&[
            "admin",
            "user",
            "add",
            "--email",
            "pm@example.com",
            "--name",
            "Pat",
            "--role",
            "tech_lead",
        ])
        .unwrap();
        assert!(matches!(
            add.command,
            Some(Command::Admin(Admin::User(UserCommand::Add {
                role: RoleArg::TechLead,
                ..
            })))
        ));
        let grant = parse(&[
            "admin",
            "user",
            "grant",
            "--email",
            "pm@example.com",
            "--projects",
            "delta,gamma",
            "--approver",
        ])
        .unwrap();
        let Some(Command::Admin(Admin::User(UserCommand::Grant {
            projects, approver, ..
        }))) = grant.command
        else {
            panic!("not a grant");
        };
        assert_eq!(projects, ["delta", "gamma"]);
        assert!(approver);
        let create = parse(&[
            "admin",
            "token",
            "create",
            "--email",
            "pm@example.com",
            "--label",
            "laptop",
        ])
        .unwrap();
        let Some(Command::Admin(Admin::Token(TokenCommand::Create { expires, .. }))) = create.command else {
            panic!("not a create");
        };
        assert_eq!(expires, TimeDelta::days(90), "the default validity");
        assert!(parse(&["admin", "token", "revoke", "k3Xq9AbCdEfG"]).is_ok());
        let quarantine = parse(&[
            "admin",
            "probe",
            "quarantine",
            "delta",
            "--reason",
            "reads too much",
        ])
        .unwrap();
        assert!(matches!(
            quarantine.command,
            Some(Command::Admin(Admin::Probe(ProbeCommand::Quarantine { ref project, ref reason })))
                if project == "delta" && reason == "reads too much"
        ));
        assert!(parse(&["admin", "probe", "release", "delta"]).is_ok());
        assert!(parse(&["admin", "probe", "list"]).is_ok());
        assert_eq!(
            parse(&["admin", "probe", "quarantine", "delta"])
                .unwrap_err()
                .exit_code(),
            2,
            "a quarantine needs a reason"
        );
        assert!(
            parse(&[
                "admin",
                "access",
                "list",
                "--project",
                "delta",
                "--email",
                "pm@example.com"
            ])
            .is_ok()
        );
        let Some(Command::Admin(Admin::Backup { to })) =
            parse(&["admin", "backup", "--to", "/data/backups/mercury-2026-10-06.db"])
                .unwrap()
                .command
        else {
            panic!("not a backup");
        };
        assert_eq!(to, PathBuf::from("/data/backups/mercury-2026-10-06.db"));
    }

    #[test]
    fn config_check_takes_a_directory() {
        let Some(Command::Admin(Admin::Config(ConfigCommand::Check { dir }))) =
            parse(&["admin", "config", "check", "--dir", "config-examples"])
                .unwrap()
                .command
        else {
            panic!("not a config check");
        };
        assert_eq!(dir, Some(PathBuf::from("config-examples")));
        assert!(matches!(
            parse(&["admin", "config", "check"]).unwrap().command,
            Some(Command::Admin(Admin::Config(ConfigCommand::Check { dir: None })))
        ));
    }

    #[test]
    fn a_wrong_command_line_is_a_usage_error() {
        for args in [
            &["serve"][..],
            &[
                "admin",
                "user",
                "add",
                "--email",
                "pm@example.com",
                "--name",
                "Pat",
                "--role",
                "approver",
            ],
            &["admin", "user", "grant", "--email", "pm@example.com"],
            &[
                "admin",
                "token",
                "create",
                "--email",
                "pm@example.com",
                "--label",
                "x",
                "--expires",
                "3w",
            ],
            &["admin", "token", "revoke"],
        ] {
            let err = parse(args).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{args:?}");
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn an_argument_that_isnt_utf_8_is_a_usage_error() {
        use std::ffi::OsString;
        #[cfg(unix)]
        let not_utf_8: OsString = std::os::unix::ffi::OsStringExt::from_vec(vec![0xff]);
        #[cfg(windows)]
        let not_utf_8: OsString = std::os::windows::ffi::OsStringExt::from_wide(&[0xd800]);
        for args in [
            vec![not_utf_8.clone()],
            ["admin", "user", "disable", "--email"]
                .map(OsString::from)
                .into_iter()
                .chain([not_utf_8])
                .collect(),
        ] {
            let args = std::iter::once(OsString::from("mercury-core")).chain(args);
            assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn validities_are_days_or_hours_above_zero() {
        assert_eq!(parse_validity("90d"), Ok(TimeDelta::days(90)));
        assert_eq!(parse_validity("7d"), Ok(TimeDelta::days(7)));
        assert_eq!(parse_validity("12h"), Ok(TimeDelta::hours(12)));
        for wrong in ["", "d", "90", "0d", "-1d", "1.5d", "3w", "90 d", "9é"] {
            assert!(parse_validity(wrong).is_err(), "{wrong:?}");
        }
    }
}
