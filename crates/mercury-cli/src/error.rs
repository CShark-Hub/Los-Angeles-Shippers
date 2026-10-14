//! What can go wrong, in words for the person at the terminal, each with what to do about it.
//! No variant carries the token, and the server's own words are shown as it sent them.

use mercury_schema::api::ErrorCode;
use mercury_schema::ids::QuestionId;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("not signed in")]
    NotSignedIn,
    #[error("MERCURY_TOKEN is set, MERCURY_URL isn't")]
    NoUrl,
    #[error("MERCURY_URL names another server than the one you signed in to")]
    OtherServer,
    #[error("{0}")]
    BadUrl(&'static str),
    #[error("that isn't a Mercury token")]
    BadToken,
    #[error("the OS keychain can't be used: {0}")]
    Keychain(&'static str),
    /// The server refused the request, with its own message and hint.
    #[error("{message}")]
    Api {
        status: u16,
        code: ErrorCode,
        message: String,
        hint: Option<String>,
    },
    /// An answer that isn't the API's error body, e.g. from a proxy in front of the server.
    #[error("the server answered {0}")]
    Status(u16),
    #[error("cannot reach {server}: {reason}")]
    Unreachable { server: String, reason: &'static str },
    #[error("the server's answer doesn't follow the API: {0}")]
    Protocol(String),
    #[error("lost the connection to the question's events")]
    Lost(QuestionId),
    /// The deadline for the question's end passed; the question may still be running.
    #[error("the question hadn't ended after {seconds} s")]
    Deadline { id: QuestionId, seconds: u64 },
    /// A question that started and then failed, in the server's words.
    #[error("the question failed: {message}")]
    Failed { code: ErrorCode, message: String },
    #[error("the question ended without an answer")]
    NoAnswer,
    #[error("the system's random numbers can't be read")]
    NoRandom,
    #[error("{0}")]
    Usage(String),
    #[error("cannot write the output: {0}")]
    Output(#[from] std::io::Error),
}

impl CliError {
    /// What the person can do about it.
    pub fn hint(&self) -> Option<String> {
        let hint = match self {
            CliError::NotSignedIn => {
                "Run `mercury login --url <server>`, or set MERCURY_URL and MERCURY_TOKEN."
            }
            CliError::NoUrl => "Set MERCURY_URL to the server's address too.",
            CliError::OtherServer => {
                "Set MERCURY_TOKEN for that server too, or unset MERCURY_URL to use your sign-in."
            }
            CliError::BadUrl(_) => "Give the server's address, like https://mercury.example.com.",
            CliError::BadToken => "Paste the whole token the admin gave you; it starts with mcy_.",
            CliError::Keychain(_) => {
                "Without a keychain (CI, containers), set MERCURY_URL and MERCURY_TOKEN."
            }
            CliError::Api { hint, .. } => return hint.clone(),
            CliError::Status(_) => "Check the server's address; something in front of it answered.",
            CliError::Unreachable { .. } => "Check the address and that the server is running.",
            CliError::Protocol(_) => "The CLI and the server may be different versions.",
            CliError::Lost(id) => return Some(format!("Follow it with `mercury ask --wait {id}`.")),
            CliError::Deadline { id, .. } => {
                return Some(format!(
                    "It may still be running: follow it with `mercury ask --wait {id}`, or raise MERCURY_ASK_DEADLINE."
                ));
            }
            CliError::Failed { code, .. } => failed_hint(*code),
            CliError::NoAnswer => "Try again; if it keeps failing, tell the admin.",
            CliError::NoRandom => return None,
            CliError::Usage(_) => "See `mercury --help`.",
            CliError::Output(_) => return None,
        };
        Some(hint.to_owned())
    }

    /// Worth trying again: the server is down or restarting, or the network is.
    pub fn is_transient(&self) -> bool {
        match self {
            CliError::Unreachable { .. } => true,
            CliError::Status(status) | CliError::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

/// What to do about a question that started and then failed, by the code the server gave: the
/// same advice for every failure would send a person who spent their day's budget to the admin
/// at once, and one whose project's data didn't answer to wait for tomorrow.
fn failed_hint(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthenticated | ErrorCode::TokenExpired => {
            "Sign in again with `mercury login --url <server>`, or ask the admin for a new token."
        }
        ErrorCode::ForbiddenProject | ErrorCode::ForbiddenClass => {
            "Ask about a project and a kind of question you may ask about; `mercury projects` lists them."
        }
        ErrorCode::RateLimited => "Wait a little and ask again: a token may ask only so often.",
        ErrorCode::BudgetExceeded => {
            "The spending limit is used up: ask again tomorrow (the day starts at midnight UTC), or tell the admin."
        }
        ErrorCode::ProbeUnavailable => {
            "The project's data isn't answering right now: try again in a few minutes; if it keeps failing, tell the admin."
        }
        ErrorCode::JudgeUnavailable => {
            "The checks that guard answers couldn't run, so nothing was shown: try again in a few minutes."
        }
        ErrorCode::InvalidRequest => "Check the question and its options; `mercury ask --help` lists them.",
        ErrorCode::Internal => "Try again; if it keeps failing, tell the admin.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [ErrorCode; 10] = [
        ErrorCode::Unauthenticated,
        ErrorCode::TokenExpired,
        ErrorCode::ForbiddenProject,
        ErrorCode::ForbiddenClass,
        ErrorCode::RateLimited,
        ErrorCode::BudgetExceeded,
        ErrorCode::ProbeUnavailable,
        ErrorCode::JudgeUnavailable,
        ErrorCode::InvalidRequest,
        ErrorCode::Internal,
    ];

    fn failed(code: ErrorCode) -> CliError {
        CliError::Failed {
            code,
            message: "The question failed.".to_owned(),
        }
    }

    #[test]
    fn a_failed_question_gets_the_hint_of_its_code() {
        let hints: Vec<String> = CODES.iter().map(|code| failed(*code).hint().unwrap()).collect();
        assert!(hints.iter().all(|hint| !hint.is_empty()));
        let mut distinct = hints.clone();
        distinct.sort();
        distinct.dedup();
        // each failure has its own advice; two codes share one only where
        // the advice is the same
        assert_eq!(distinct.len(), 8, "{hints:#?}");
        assert!(
            failed(ErrorCode::BudgetExceeded)
                .hint()
                .unwrap()
                .contains("tomorrow")
        );
        assert!(
            failed(ErrorCode::ProbeUnavailable)
                .hint()
                .unwrap()
                .contains("data")
        );
        assert!(failed(ErrorCode::TokenExpired).hint().unwrap().contains("login"));
        assert!(failed(ErrorCode::RateLimited).hint().unwrap().contains("Wait"));
    }

    #[test]
    fn an_internal_failure_and_no_answer_keep_the_old_advice() {
        let old = "Try again; if it keeps failing, tell the admin.";
        assert_eq!(failed(ErrorCode::Internal).hint().as_deref(), Some(old));
        assert_eq!(CliError::NoAnswer.hint().as_deref(), Some(old));
    }
}
