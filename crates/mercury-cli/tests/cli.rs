//! The `mercury` binary against a stand-in for the core's API: the timeline, the result, the
//! `--json` object and the exit codes of each way a question can end, a broken stream resumed,
//! sign-in and sign-out, and never the token in any output.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

mod stub;

use std::io::Cursor;
use std::process::{Command, Stdio};

use mercury_cli::cli::Command as CliCommand;
use mercury_cli::credentials::{Login, MemoryStore, Server, token};
use mercury_cli::run::{Io, run};
use serde_json::{Value, json};
use stub::{ADMIN_SECRET, ADMIN_TOKEN, ANSWER, APPROVAL, OTHER_TOKEN, SECRET, TOKEN};

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Output {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|err| panic!("not one JSON object ({err}):\n{}", self.stdout))
    }
}

fn assert_no_token(texts: &[&str]) {
    for text in texts {
        for secret in [SECRET, ADMIN_SECRET] {
            assert!(
                !text.contains(secret),
                "a token's secret is in the output:\n{text}"
            );
        }
        assert!(!text.contains(OTHER_TOKEN), "a token is in the output:\n{text}");
    }
}

/// Runs the binary with the stand-in's address and `token` from the environment, never the keychain,
/// and `extra` variables besides. The deadline variable is only ever what `extra` sets.
fn mercury_env(url: &str, token: &str, extra: &[(&str, &str)], args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_mercury"))
        .args(args)
        .env_remove("MERCURY_ASK_DEADLINE")
        .env("MERCURY_URL", url)
        .env("MERCURY_TOKEN", token)
        .envs(extra.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let output = Output {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    assert_no_token(&[&output.stdout, &output.stderr]);
    output
}

fn mercury_with(url: &str, token: &str, args: &[&str]) -> Output {
    mercury_env(url, token, &[], args)
}

fn mercury(url: &str, args: &[&str]) -> Output {
    mercury_with(url, TOKEN, args)
}

#[test]
fn audit_prints_a_line_per_row_and_follows_the_cursor() {
    let core = stub::start();
    let out = mercury_with(
        &core.url,
        ADMIN_TOKEN,
        &["audit", "--project", "delta", "--since", "2026-10-01"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{}", out.stdout);
    assert!(lines[0].starts_with("SEQ") && lines[0].contains("QUESTION") && lines[0].ends_with("USER"));
    assert!(
        lines[1].starts_with("1834  2026-10-04 15:12:03  classified"),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains("accepted (payload expired)"), "{}", lines[2]);
    assert!(lines[3].starts_with("1836") && lines[3].ends_with("dana.reyes@example.com"));
    let seen = core.seen.lock().unwrap();
    assert_eq!(seen.audits.len(), 2, "two pages");
    assert!(
        seen.audits[0].contains("project=delta") && seen.audits[0].contains("from=2026-10-01T00%3A00%3A00Z")
    );
    assert!(seen.audits[1].contains("cursor=1835"), "{:?}", seen.audits);
}

/// The audit is an admin's alone: a manager's `mercury audit` prints no rows, only the server's
/// refusal in its words with its hint, and exits 1.
#[test]
fn a_managers_audit_is_refused_with_the_servers_words_and_exit_1() {
    let core = stub::start();
    let out = mercury(&core.url, &["audit", "--project", "delta"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(out.stdout, "", "no rows, not even the header");
    let (message, hint) = stub::AUDIT_REFUSED;
    assert_eq!(out.stderr, format!("mercury: {message}\n  {hint}\n"));
    assert_eq!(
        core.seen.lock().unwrap().audits,
        ["/v1/audit?project=delta&limit=500"],
        "asked once, not paged on"
    );
}

#[test]
fn approvals_lists_each_held_answer_with_its_flags_and_who_may_decide() {
    let core = stub::start();
    let out = mercury(&core.url, &["approvals"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = &out.stdout;
    let blocks: Vec<&str> = text.split("\n\n").collect();
    assert_eq!(blocks.len(), 2, "{text}");
    let (theirs, own) = (blocks[0], blocks[1]);
    assert!(
        theirs.starts_with(&format!(
            "{} · delta · progress · asked by a pm · until 2026-10-05 15:12 UTC",
            stub::APPROVAL
        )),
        "{text}"
    );
    assert!(
        theirs.contains("  Waits because an output check landed in its review band."),
        "{text}"
    );
    assert!(
        theirs.contains("  Draft: Export is done. The CsvWriter part is tested."),
        "one line: {text}"
    );
    assert!(
        theirs.contains(
            "  Flagged: \"The CsvWriter part is tested.\" (sentence_contains_code_or_identifier, p 0.60)"
        ),
        "{text}"
    );
    assert!(
        theirs.contains(&format!("mercury approvals --approve {}", stub::APPROVAL)),
        "{text}"
    );

    // the caller's own question: that it waits for another approver, and nothing of its draft
    assert_eq!(
        own,
        format!(
            "{} · delta · progress · asked by a pm · until 2026-10-05 15:12 UTC
  Waits because an output check landed in its review band.
  Your own question: it waits for another approver of the project, and only they see the draft.
  Follow it with: mercury ask --wait {}
",
            stub::OWN_APPROVAL,
            stub::OWN_QUESTION
        )
    );
    assert_eq!(core.seen.lock().unwrap().approval_lists, ["status=pending"]);

    // and the same in JSON: the own item keeps its id and state, and comes without a draft or flags
    let json = mercury(&core.url, &["approvals", "--json"]);
    assert_eq!(json.code, 0, "{}", json.stderr);
    let list = json.json();
    let own = &list["approvals"][1];
    assert_eq!(own["id"], stub::OWN_APPROVAL);
    assert_eq!(own["question_id"], stub::OWN_QUESTION);
    assert_eq!(own["asked_by_me"], true);
    assert_eq!(own["status"], "pending");
    assert!(own.get("draft").is_none() && own.get("flags").is_none(), "{own}");
    let theirs = &list["approvals"][0];
    assert_eq!(theirs["asked_by_me"], false);
    assert_eq!(
        theirs["draft"]["text"],
        "Export is done.\nThe CsvWriter part is tested."
    );
    assert_eq!(theirs["flags"].as_array().map(Vec::len), Some(1));
}

#[test]
fn approvals_approves_and_rejects_with_a_note_and_says_why_it_cant() {
    let core = stub::start();
    let out = mercury(&core.url, &["approvals", "--approve", stub::APPROVAL]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "Approved: the asker gets the answer exactly as the list showed it."
    );
    let note = "Ask the tech lead about the export first.";
    let out = mercury(
        &core.url,
        &["approvals", "--reject", stub::APPROVAL, "--note", note],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        "Rejected: the asker gets the refusal with your note."
    );
    let out = mercury(&core.url, &["approvals", "--approve", stub::OWN_APPROVAL]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("You can't decide on your own question."),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("Another approver of the project decides on it."),
        "{}",
        out.stderr
    );
    let seen = core.seen.lock().unwrap();
    assert_eq!(
        seen.decisions,
        [
            (stub::APPROVAL.to_owned(), json!({ "decision": "approve" })),
            (
                stub::APPROVAL.to_owned(),
                json!({ "decision": "reject", "note": note })
            ),
            (stub::OWN_APPROVAL.to_owned(), json!({ "decision": "approve" })),
        ]
    );
}

#[test]
fn approvals_refuses_a_command_line_that_doesnt_say_one_thing() {
    let core = stub::start();
    for args in [
        &["approvals", "--reject", stub::APPROVAL][..],
        &["approvals", "--note", "why"],
        &[
            "approvals",
            "--approve",
            stub::APPROVAL,
            "--reject",
            stub::APPROVAL,
            "--note",
            "why",
        ],
        &["approvals", "--approve", stub::APPROVAL, "--json"],
        &["approvals", "--approve", "q_01J9Z3QK7M8N9P0R1S2T3V0001"],
    ] {
        let out = mercury(&core.url, args);
        assert_eq!(out.code, 1, "{args:?}: {}", out.stdout);
    }
    assert!(core.seen.lock().unwrap().decisions.is_empty(), "nothing was sent");
}

#[test]
fn audit_export_writes_the_file_and_a_refused_one_leaves_none() {
    let core = stub::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("audit.jsonl");
    let path = file.to_str().unwrap();
    let out = mercury_with(
        &core.url,
        ADMIN_TOKEN,
        &["audit", "--project", "delta", "--export", path],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.trim(), format!("Exported 2 row(s) to {path}."));
    let written = std::fs::read_to_string(&file).unwrap();
    let rows: Vec<Value> = written
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["payload"], Value::Null);
    assert!(core.seen.lock().unwrap().audits[0].contains("format=jsonl"));

    // a manager's export is refused
    let refused = dir.path().join("refused.jsonl");
    let out = mercury(
        &core.url,
        &[
            "audit",
            "--project",
            "delta",
            "--export",
            refused.to_str().unwrap(),
        ],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("Only an admin may export the audit."),
        "{}",
        out.stderr
    );
    assert!(!refused.exists(), "no half file is left");
}

/// What was at the export's path is never truncated by an export that fails, and never deleted: the
/// export is written beside it and moved over it once whole.
#[test]
fn an_export_never_touches_a_file_it_did_not_write() {
    let core = stub::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("audit.jsonl");
    std::fs::write(&file, "an earlier export\n").unwrap();
    let path = file.to_str().unwrap();

    // refused (403, a manager's): the earlier file is as it was, and nothing else is left in the
    // directory
    let out = mercury(&core.url, &["audit", "--project", "delta", "--export", path]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "an earlier export\n");
    assert_eq!(names(dir.path()), ["audit.jsonl"], "no half file");

    // a whole export, the admin's, replaces it
    let out = mercury_with(
        &core.url,
        ADMIN_TOKEN,
        &["audit", "--project", "delta", "--export", path],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written.lines().count(), 2, "{written}");
    assert!(!written.contains("an earlier export"));
    assert_eq!(
        names(dir.path()),
        ["audit.jsonl"],
        "the part was moved, not copied"
    );

    // a path that names no file
    assert_eq!(
        mercury(&core.url, &["audit", "--export", ".."]).code,
        1,
        "--export needs the name of a file"
    );
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// A server that stops sending mid-export ends the command with an error after the idle limit, and
/// leaves what was at the path alone.
#[test]
fn a_server_that_stalls_mid_export_ends_it_and_leaves_no_file() {
    let core = stub::start();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("audit.jsonl");
    std::fs::write(&file, "an earlier export\n").unwrap();
    let client = mercury_cli::client::Client::new(&Login {
        server: Server::parse(&core.url).unwrap(),
        token: token(ADMIN_TOKEN).unwrap(),
    })
    .unwrap()
    .with_idle(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = runtime
        .block_on(mercury_cli::run::export_to(
            &client,
            &[("project", "stall".to_owned())],
            &file,
        ))
        .unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "it stopped at the idle limit: {:?}",
        started.elapsed()
    );
    assert!(err.to_string().contains("the export stopped sending"), "{err}");
    assert!(err.is_transient(), "worth trying again");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "an earlier export\n");
    assert_eq!(names(dir.path()), ["audit.jsonl"], "no half file");
}

/// The unit of `--since` is its last character, which may take several bytes: that is a mistake of
/// the command line (exit 1), never a panic (exit 101).
#[test]
fn a_since_with_a_multibyte_unit_is_a_usage_error_not_a_panic() {
    let core = stub::start();
    for bad in ["7é", "café", "7€"] {
        let out = mercury(&core.url, &["audit", "--since", bad]);
        assert_eq!(out.code, 1, "{bad}: {}", out.stderr);
        assert!(!out.stderr.contains("panicked"), "{bad}: {}", out.stderr);
        assert!(out.stderr.contains("give a date like"), "{bad}: {}", out.stderr);
    }
    assert!(core.seen.lock().unwrap().audits.is_empty(), "nothing was sent");
}

/// The core's "audit write fails" mode: the question is `failed` and its stream closes with no
/// terminal event. Resuming would never bring one, so asking looks at the question's state.
#[test]
fn a_stream_that_closes_without_an_end_for_a_failed_question_exits_1() {
    let core = stub::start();
    let started = std::time::Instant::now();
    let out = mercury(&core.url, &["ask", "--project", "delta", "vanishes", "--json"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "no resumes with their pauses: {:?}",
        started.elapsed()
    );
    let json = out.json();
    assert_eq!(json["status"], "failed");
    assert_eq!(json["error"]["code"], "internal");
    assert!(out.stderr.contains("the question failed"), "{}", out.stderr);
    assert!(
        !out.stderr.contains("lost the connection"),
        "it says how the question ended, not that the connection was lost:\n{}",
        out.stderr
    );
    let seen = core.seen.lock().unwrap();
    assert_eq!(seen.streams.len(), 1, "one stream, not resumed");
    assert_eq!(seen.states.len(), 1, "the state was asked once");
}

#[test]
fn an_answer_exits_0_with_the_timeline_on_stderr_and_the_answer_on_stdout() {
    let core = stub::start();
    let out = mercury(
        &core.url,
        &["ask", "--project", "delta", "what was merged this week?"],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    for line in [
        "asked      q_",
        "project    delta (as asked)",
        "class      progress (p 0.94)",
        "check      asks_for_code pass (jev, p 0.02)",
        "plan       progress(since 2026-09-28)",
        "call 1     progress: arguments_valid pass (rule)",
        "result 1   progress: 2 items, access code",
        "judged     2 sentences, 0 stripped, 0 held for sign-off",
        "answer     confidence 0.86, 2 facts",
        "done       6.4 s · $0.0110 · judge: jev · composer: model · audit 1834",
    ] {
        assert!(
            out.stderr.contains(line),
            "{line:?} missing from the timeline:\n{}",
            out.stderr
        );
    }
    assert!(out.stdout.starts_with(ANSWER), "{}", out.stdout);
    assert!(out.stdout.contains("Facts (2):"));
    assert!(
        out.stdout
            .contains("· Export invoices to CSV · Export · done · tests passing · updated 2026-10-01")
    );
    assert!(
        !out.stderr.contains(ANSWER),
        "the answer's text belongs on stdout only"
    );
}

#[test]
fn json_is_one_object_with_the_final_state() {
    let core = stub::start();
    let out = mercury(
        &core.url,
        &[
            "ask",
            "--project",
            "delta",
            "what was merged this week?",
            "--json",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["status"], "delivered");
    assert_eq!(json["project"], "delta");
    assert_eq!(json["class"], "progress");
    assert_eq!(json["answer"]["text"], ANSWER);
    assert_eq!(json["answer"]["facts"].as_array().map(Vec::len), Some(2));
    assert_eq!(json["plan"][0]["tool"], "progress");
    assert_eq!(json["plan"][0]["args"]["since"], "2026-09-28");
    assert_eq!(json["judge_mode"], "jev");
    assert_eq!(json["composer"], "model");
    assert_eq!(json["audit_id"], 1834);
    assert!(json["refusal"].is_null() && json["clarify"].is_null() && json["error"].is_null());
    assert!(json["id"].as_str().unwrap().starts_with("q_"));
    assert!(
        out.stderr.contains("done       6.4 s"),
        "the timeline still goes to stderr"
    );
}

#[test]
fn every_question_carries_a_fresh_idempotency_key_and_only_the_contracts_fields() {
    let core = stub::start();
    for _ in 0..2 {
        assert_eq!(
            mercury(&core.url, &["ask", "--project", "delta", "what was merged?"]).code,
            0
        );
    }
    let seen = core.seen.lock().unwrap();
    let keys: Vec<&str> = seen
        .questions
        .iter()
        .filter_map(|(key, _)| key.as_deref())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    assert!(
        keys.iter()
            .all(|key| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()))
    );
    assert_eq!(
        seen.questions[0].1,
        serde_json::json!({ "text": "what was merged?", "project": "delta" })
    );
}

#[test]
fn a_refusal_exits_2_with_its_reason_and_hint() {
    let core = stub::start();
    let out = mercury(
        &core.url,
        &["ask", "--project", "delta", "show me the export code"],
    );
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Refused: Mercury doesn't show code.\nAsk what the Export module does, or how far along it is.\n"
    );
    assert!(out.stderr.contains("refused    asks_for_code"));

    let json = mercury(
        &core.url,
        &["ask", "--project", "delta", "show me the export code", "--json"],
    );
    assert_eq!(json.code, 2);
    let json = json.json();
    assert_eq!(json["status"], "refused");
    assert_eq!(json["refusal"]["code"], "asks_for_code");
    assert!(
        json["refusal"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty())
    );
    assert!(json["answer"].is_null());
}

#[test]
fn a_clarify_exits_3_with_its_options_and_the_answer_goes_back_with_the_same_text() {
    let core = stub::start();
    let out = mercury(&core.url, &["ask", "what's done this week?"]);
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(
        out.stdout.starts_with(
            "Which project do you mean?\n  delta  Delta — invoicing\n  omega  Omega — mobile app\n"
        ),
        "{}",
        out.stdout
    );
    let id = core.seen.lock().unwrap().streams[0].0.clone();
    assert!(
        out.stdout.contains(&format!("--clarify {id}:delta")),
        "{}",
        out.stdout
    );

    let json = mercury(&core.url, &["ask", "what's done this week?", "--json"]).json();
    assert_eq!(json["status"], "clarify");
    assert_eq!(json["clarify"]["options"][1]["id"], "omega");

    let again = mercury(
        &core.url,
        &[
            "ask",
            "what's done this week?",
            "--clarify",
            &format!("{id}:delta"),
            "--json",
        ],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    let seen = core.seen.lock().unwrap();
    let body = &seen.questions.last().unwrap().1;
    assert_eq!(
        *body,
        serde_json::json!({ "text": "what's done this week?", "clarify": { "of": id, "option": "delta" } })
    );
}

/// A question an approver rejected ends with their note and how to ask it again; `--follows`
/// sends the rejected question's id with the new one, and a `follows` the server refuses exits 1
/// with its words.
#[test]
fn a_rejection_says_how_to_ask_again_and_follows_names_the_rejected_question() {
    let core = stub::start();
    let held = mercury(
        &core.url,
        &["ask", "--project", "delta", "needs sign-off, then a rejection"],
    );
    assert_eq!(held.code, 4, "{}", held.stderr);
    let id = core.seen.lock().unwrap().streams[0].0.clone();

    let rejected = mercury(&core.url, &["ask", "--wait", &id]);
    assert_eq!(rejected.code, 2, "{}", rejected.stderr);
    assert_eq!(
        rejected.stdout,
        format!(
            "Refused: {}\n{}\nAsk again, in new words or the same, naming the rejected question:\n  mercury ask \"<your question>\" --follows {id}\n",
            stub::REJECTED.0,
            stub::REJECTED.1
        )
    );

    let again = mercury(
        &core.url,
        &[
            "ask",
            "--project",
            "delta",
            "--follows",
            &id,
            "what is done in billing?",
            "--json",
        ],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(again.json()["status"], "delivered");
    let body = core.seen.lock().unwrap().questions.last().unwrap().1.clone();
    assert_eq!(
        body,
        json!({ "text": "what is done in billing?", "project": "delta", "follows": id })
    );

    // a question that wasn't rejected: the server's refusal, its words, exit 1
    let other = core.seen.lock().unwrap().streams.last().unwrap().0.clone();
    let refused = mercury(
        &core.url,
        &["ask", "--project", "delta", "--follows", &other, "and now?"],
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(stub::FOLLOWS_REFUSED),
        "{}",
        refused.stderr
    );
}

#[test]
fn a_pending_sign_off_exits_4_and_wait_follows_it_to_the_answer() {
    let core = stub::start();
    let out = mercury(&core.url, &["ask", "--project", "delta", "needs sign-off"]);
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert!(
        out.stdout.contains(&format!("approval {APPROVAL}")),
        "{}",
        out.stdout
    );
    let id = core.seen.lock().unwrap().streams[0].0.clone();
    assert!(
        out.stdout.contains(&format!("mercury ask --wait {id}")),
        "{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains(ANSWER) && !out.stderr.contains(ANSWER),
        "nothing of a held answer shows"
    );
    assert!(
        out.stderr
            .contains("judged     2 sentences, 0 stripped, 1 held for sign-off")
    );

    let waited = mercury(&core.url, &["ask", "--wait", &id, "--json"]);
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    let json = waited.json();
    assert_eq!(json["status"], "delivered");
    assert_eq!(json["answer"]["text"], ANSWER);
    assert_eq!(json["signoff"]["approval_id"], APPROVAL);
    let seen = core.seen.lock().unwrap();
    let resumed: Vec<Option<&str>> = seen
        .streams
        .iter()
        .skip(1)
        .map(|(_, last)| last.as_deref())
        .collect();
    assert_eq!(
        resumed,
        [None, Some("9")],
        "--wait replays, then resumes after the sign-off"
    );
}

#[test]
fn a_broken_stream_is_resumed_after_the_last_event() {
    let core = stub::start();
    let out = mercury(&core.url, &["ask", "--project", "delta", "drops", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["status"], "delivered");
    let seen = core.seen.lock().unwrap();
    let lasts: Vec<Option<&str>> = seen.streams.iter().map(|(_, last)| last.as_deref()).collect();
    assert_eq!(lasts, [None, Some("3")]);
    assert_eq!(
        out.stderr.matches("class      progress").count(),
        1,
        "no event shown twice"
    );
}

/// A question whose stream stays open on keep-alives and never ends must not hang a script: asking
/// stops after the deadline, exits 1 and prints the state as it stands, and the question can be
/// followed later.
#[test]
fn a_question_that_stopped_streaming_ends_at_the_deadline_with_its_state() {
    let core = stub::start();
    let started = std::time::Instant::now();
    let out = mercury_env(
        &core.url,
        TOKEN,
        &[("MERCURY_ASK_DEADLINE", "2")],
        &["ask", "--project", "delta", "stalls", "--json"],
    );
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "it stopped at the deadline, not at the stand-in's end: {:?}",
        started.elapsed()
    );
    let json = out.json();
    assert_eq!(json["status"], "running", "the state as it stands");
    assert_eq!(json["project"], "delta", "what the events told before it stalled");
    assert!(json["answer"].is_null() && json["error"].is_null());
    let id = json["id"].as_str().unwrap();
    assert!(
        out.stderr.contains("the question hadn't ended after 2 s"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&format!("mercury ask --wait {id}"))
            && out.stderr.contains("MERCURY_ASK_DEADLINE"),
        "it says how to go on:\n{}",
        out.stderr
    );
}

/// Without the variable the deadline is two minutes: a question that ends is never cut short, and a
/// wrong value is a mistake of the command line, found before anything is asked.
#[test]
fn the_deadline_is_a_whole_number_of_seconds_and_a_wrong_one_asks_nothing() {
    let core = stub::start();
    for wrong in ["soon", "0", "-5", "1.5", "86401"] {
        let out = mercury_env(
            &core.url,
            TOKEN,
            &[("MERCURY_ASK_DEADLINE", wrong)],
            &["ask", "--project", "delta", "what was merged this week?"],
        );
        assert_eq!(out.code, 1, "{wrong}: {}", out.stderr);
        assert!(
            out.stderr
                .contains("MERCURY_ASK_DEADLINE must be a whole number of seconds"),
            "{wrong}: {}",
            out.stderr
        );
    }
    assert!(
        core.seen.lock().unwrap().questions.is_empty(),
        "nothing was asked"
    );

    // set to a good value, or to nothing, an answer still comes
    for good in ["30", " 30 ", ""] {
        let out = mercury_env(
            &core.url,
            TOKEN,
            &[("MERCURY_ASK_DEADLINE", good)],
            &["ask", "--project", "delta", "what was merged this week?"],
        );
        assert_eq!(out.code, 0, "{good:?}: {}", out.stderr);
    }
}

#[test]
fn an_error_event_exits_1_with_its_message() {
    let core = stub::start();
    let out = mercury(&core.url, &["ask", "--project", "delta", "fails"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr
            .contains("error      probe_unavailable: The project's probe can't answer right now.")
    );
    assert!(
        out.stderr
            .contains("mercury: the question failed: The project's probe can't answer right now.")
    );

    let json = mercury(&core.url, &["ask", "--project", "delta", "fails", "--json"]);
    assert_eq!(json.code, 1);
    let json = json.json();
    assert_eq!(json["status"], "failed");
    assert_eq!(json["error"]["code"], "probe_unavailable");
}

#[test]
fn a_rejected_token_exits_1_with_the_servers_words_and_no_token_anywhere() {
    let core = stub::start();
    let out = mercury_with(
        &core.url,
        OTHER_TOKEN,
        &["ask", "--project", "delta", "what was merged?"],
    );
    assert_eq!(out.code, 1);
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "mercury: The request has no valid token.\n  Sign in again with your token, or ask the admin for a new one.\n"
    );
    let json = mercury_with(&core.url, OTHER_TOKEN, &["ask", "what was merged?", "--json"]);
    assert_eq!(json.code, 1);
    assert_eq!(json.json()["error"]["code"], "unauthenticated");
    assert!(core.seen.lock().unwrap().questions.is_empty());
}

#[test]
fn a_server_that_isnt_there_exits_1_and_says_so() {
    let out = mercury("http://127.0.0.1:9", &["projects"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .starts_with("mercury: cannot reach http://127.0.0.1:9: "),
        "{}",
        out.stderr
    );
}

#[test]
fn me_and_projects_show_who_and_what() {
    let core = stub::start();
    let me = mercury(&core.url, &["me"]);
    assert_eq!(me.code, 0, "{}", me.stderr);
    assert_eq!(
        me.stdout,
        format!(
            "Signed in to {} as Dana Reyes (pm), dana.reyes@example.com\nToken AbCdEf123456, valid until 2026-12-21 00:00 UTC\nProjects: delta, omega (approver)\n",
            core.url
        )
    );
    assert_eq!(mercury(&core.url, &["me", "--json"]).json()["user"]["role"], "pm");

    let projects = mercury(&core.url, &["projects"]);
    assert_eq!(projects.code, 0, "{}", projects.stderr);
    assert_eq!(
        projects.stdout,
        "\
delta  Delta — invoicing
  probe    healthy, data as of 2026-10-02 11:34 UTC
  classes  progress, tested, blocked, health, changes
  modules  Invoicing, Export, Auth

omega  Omega — mobile app
  probe    not registered yet
  classes  progress
  modules  none
"
    );
}

#[test]
fn the_command_line_has_its_own_exit_codes() {
    let url = "http://127.0.0.1:9";
    let help = mercury(url, &["--help"]);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("4  waiting for an approver's sign-off"));
    let version = mercury(url, &["--version"]);
    assert_eq!((version.code, version.stdout.as_str()), (0, "mercury 0.0.0\n"));
    for wrong in [
        &[][..],
        &["ask"],
        &["ask", "  "],
        &["nothing"],
        &["ask", "what?", "--project", "Delta"],
    ] {
        assert_eq!(mercury(url, wrong).code, 1, "{wrong:?}");
    }
}

#[test]
fn plain_http_to_another_machine_is_refused_before_any_request() {
    let out = mercury("http://mercury.example.com", &["me"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("use https://"), "{}", out.stderr);
}

/// The library's `run`, with a store in memory instead of the keychain.
fn run_with(store: &MemoryStore, command: CliCommand, stdin: &str) -> Output {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(
        command,
        Io {
            var: &|_| None,
            store,
            stdin: &mut Cursor::new(stdin.as_bytes().to_vec()),
            stdin_is_terminal: false,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    let output = Output {
        code: i32::from(code),
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    };
    assert_no_token(&[&output.stdout, &output.stderr]);
    output
}

fn login(url: &str, token: Option<&str>) -> CliCommand {
    CliCommand::Login {
        url: url.to_owned(),
        token: token.map(str::to_owned),
    }
}

#[test]
fn login_checks_the_token_then_keeps_it_and_logout_removes_it() {
    let core = stub::start();
    let store = MemoryStore::default();
    let out = run_with(&store, login(&core.url, None), &format!("{TOKEN}\n"));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        format!(
            "Signed in to {} as Dana Reyes (pm). The token is in the OS keychain.\n",
            core.url
        )
    );
    let kept = store.current().unwrap();
    assert_eq!(
        (kept.server.as_str(), kept.token.expose()),
        (core.url.as_str(), TOKEN)
    );

    let me = run_with(&store, CliCommand::Me { json: false }, "");
    assert_eq!(me.code, 0, "the kept sign-in is used: {}", me.stderr);

    let out = run_with(&store, CliCommand::Logout, "");
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (0, "Signed out: the token is gone from the OS keychain.\n")
    );
    assert!(store.current().is_none());
    let again = run_with(&store, CliCommand::Logout, "");
    assert_eq!(again.stdout, "Not signed in: there was nothing to remove.\n");
    let me = run_with(&store, CliCommand::Me { json: false }, "");
    assert_eq!(me.code, 1);
    assert!(me.stderr.starts_with("mercury: not signed in\n"), "{}", me.stderr);
}

#[test]
fn a_token_the_server_refuses_is_never_kept() {
    let core = stub::start();
    let store = MemoryStore::default();
    let out = run_with(&store, login(&core.url, Some(OTHER_TOKEN)), "");
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.starts_with("mercury: The request has no valid token."),
        "{}",
        out.stderr
    );
    assert!(store.current().is_none());

    let out = run_with(&store, login(&core.url, None), "not a token\n");
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.starts_with("mercury: that isn't a Mercury token"),
        "{}",
        out.stderr
    );
    assert!(store.current().is_none());
}

#[test]
fn login_refuses_plain_http_to_another_machine() {
    let store = MemoryStore::default();
    let out = run_with(&store, login("http://mercury.example.com", Some(TOKEN)), "");
    assert_eq!(out.code, 1);
    assert!(store.current().is_none());
}

#[test]
fn a_kept_sign_in_never_goes_to_another_server() {
    let core = stub::start();
    let store = MemoryStore::holding(Login {
        server: Server::parse(&core.url).unwrap(),
        token: token(TOKEN).unwrap(),
    });
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = run(
        CliCommand::Me { json: false },
        Io {
            var: &|name| (name == "MERCURY_URL").then(|| "https://elsewhere.example.com".to_owned()),
            store: &store,
            stdin: &mut Cursor::new(Vec::new()),
            stdin_is_terminal: false,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    assert_eq!(code, 1);
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.starts_with("mercury: MERCURY_URL names another server"),
        "{stderr}"
    );
}
