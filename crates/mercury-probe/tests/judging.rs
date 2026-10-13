//! The probe's questions to the core's judge, on the fixture repository: a stand-in core answers
//! (or doesn't), and the index shows what the probe made of it. No answer is never an answer: a
//! conflict stays `unclear`, a kind `unknown`, and a title that waited for the judge is withheld,
//! as is a title only the core's rules answered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use mercury_probe::index::Index;
use mercury_probe::judging::{JudgeClient, MAX_RETRY_AFTER};
use mercury_probe::probe::{LiveIndex, REFRESH_EVERY, RULES_RECHECK, Source};
use mercury_schema::facts::{Kind, Status, WaitingOn};
use mercury_schema::probe_judge::{JudgeRequest, Purpose};
use mercury_schema::{Fact, JudgeMode};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "probe-delta-token-for-tests";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The fixture in a folder of its own, so a test can change it.
fn own_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    (dir, repo)
}

/// A stand-in for the core's judge, answering in `mode`: `blocked` at `status_p` for a status,
/// `fix` at `kind_p` for a kind, and for a title 0.97 when it holds `flag`, else `clean_p`.
struct Core {
    mode: JudgeMode,
    flag: &'static str,
    status_p: f64,
    kind_p: f64,
    clean_p: f64,
}

impl Core {
    /// The core when no judge answers: its rules, which can't tell a status (two candidates) or a
    /// kind (three), and find nothing in a title but `flag`.
    fn rules(flag: &'static str) -> Self {
        Self {
            mode: JudgeMode::Rules,
            flag,
            status_p: 0.5,
            kind_p: 1.0 / 3.0,
            clean_p: 0.0,
        }
    }
}

impl Respond for Core {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Ok(asked) = serde_json::from_slice::<JudgeRequest>(&request.body) else {
            return ResponseTemplate::new(400);
        };
        let mode = serde_json::to_value(self.mode).unwrap();
        let reply = match asked {
            JudgeRequest::StatusFromCandidates { items } => json!({
                "purpose": "status_from_candidates", "judge_mode": mode,
                "items": items.iter()
                    .map(|i| json!({ "id": i.id, "status": "blocked", "p": self.status_p }))
                    .collect::<Vec<_>>()
            }),
            JudgeRequest::Kind { items } => json!({
                "purpose": "kind", "judge_mode": mode,
                "items": items.iter()
                    .map(|i| json!({ "id": i.id, "kind": "fix", "p": self.kind_p }))
                    .collect::<Vec<_>>()
            }),
            JudgeRequest::TitleSafety { items } => json!({
                "purpose": "title_safety", "judge_mode": mode,
                "items": items.iter().map(|i| {
                    let p = if i.title.as_str().contains(self.flag) { 0.97 } else { self.clean_p };
                    json!({ "id": i.id, "p": p })
                }).collect::<Vec<_>>()
            }),
        };
        ResponseTemplate::new(200).set_body_json(reply)
    }
}

async fn core_with(mode: JudgeMode, flag: &'static str, status_p: f64, clean_p: f64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(Core {
            mode,
            flag,
            status_p,
            kind_p: 0.9,
            clean_p,
        })
        .mount(&server)
        .await;
    server
}

async fn core(mode: JudgeMode, flag: &'static str) -> MockServer {
    core_with(mode, flag, 0.81, 0.02).await
}

/// The index of `repo` in `dev` mode, asking `core`'s judge.
async fn judged_index(repo: &Path, core: &MockServer) -> LiveIndex {
    let source = Source::Dev {
        repo: repo.to_owned(),
        meta: repo.join("meta"),
        token: None,
    };
    let client = JudgeClient::new(&core.uri(), Some(TOKEN.to_owned())).unwrap();
    LiveIndex::new(Index::in_memory().await.unwrap(), source).with_judge(client)
}

/// The facts by the number of their pull request or issue.
async fn by_number(live: &LiveIndex) -> BTreeMap<u32, Fact> {
    live.index
        .facts()
        .await
        .unwrap()
        .into_iter()
        .map(|fact| (fact.evidence[0].reference.unwrap().get(), fact))
        .collect()
}

/// The requests the stand-in received: each one's purpose and its items.
async fn received(core: &MockServer) -> Vec<(Purpose, Vec<Value>)> {
    core.received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            let asked: JudgeRequest = serde_json::from_slice(&request.body).unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            (asked.purpose(), body["items"].as_array().unwrap().clone())
        })
        .collect()
}

fn count(requests: &[(Purpose, Vec<Value>)], purpose: Purpose) -> usize {
    requests
        .iter()
        .filter(|(p, _)| *p == purpose)
        .map(|(_, items)| items.len())
        .sum()
}

/// The kinds the fixture's labels don't settle: #415 (`export`), #427 (`export`), #432 (none).
const KIND_OPEN: [u32; 3] = [415, 427, 432];
/// The titles the rules already withhold: an identifier, a backtick, a path, a URL.
const RULES_WITHHOLD: [u32; 4] = [419, 424, 426, 427];

#[tokio::test]
async fn a_conflict_is_settled_by_the_judges_choice_and_open_kinds_by_its_pick() {
    let core = core(JudgeMode::Jev, "never in a title").await;
    let mut live = judged_index(&fixture(), &core).await;
    assert!(live.refresh().await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert!(report.complete(), "{report:?}");
    let facts = by_number(&live).await;
    // #422 is in review and fixes the blocked #407: the judge picks blocked
    let pull = &facts[&422];
    assert_eq!(pull.status, Status::Blocked);
    assert!((pull.confidence.get() - 0.81).abs() < 1e-9);
    assert_eq!(
        pull.waiting_on,
        Some(WaitingOn::Unknown),
        "the issue's label is `blocked`"
    );
    assert_eq!(pull.kind, Kind::Fix, "its `bug` label settles it");
    for number in KIND_OPEN {
        assert_eq!(facts[&number].kind, Kind::Fix, "#{number}: the judge's pick");
    }
    assert_eq!(facts[&421].kind, Kind::Feature, "a `feature` label, never asked");
    assert_eq!(facts[&425].kind, Kind::Chore);
    // nothing flagged: every title the rules let through stays
    for (number, fact) in &facts {
        assert_eq!(fact.title_withheld, RULES_WITHHOLD.contains(number), "#{number}");
    }

    let requests = received(&core).await;
    assert_eq!(requests.len(), 3, "one request per purpose");
    assert_eq!(count(&requests, Purpose::TitleSafety), 19 - RULES_WITHHOLD.len());
    assert_eq!(count(&requests, Purpose::StatusFromCandidates), 1);
    assert_eq!(count(&requests, Purpose::Kind), KIND_OPEN.len());
    let (_, status) = requests
        .iter()
        .find(|(p, _)| *p == Purpose::StatusFromCandidates)
        .unwrap();
    assert_eq!(status[0]["candidates"], json!(["in_review", "blocked"]));
    assert_eq!(status[0]["signals"]["pr"]["review_requested"], true);
    assert_eq!(
        status[0]["signals"]["issues"],
        json!([{ "open": true, "blocked": true }])
    );
}

#[tokio::test]
async fn a_title_the_judge_flags_is_withheld_and_not_sent_again() {
    let core = core(JudgeMode::Jev, "Faster statement").await;
    let mut live = judged_index(&fixture(), &core).await;
    live.refresh().await.unwrap();
    let facts = by_number(&live).await;
    let flagged = &facts[&415];
    assert_eq!(
        (flagged.title.as_str(), flagged.title_withheld),
        ("Work item in Export", true)
    );
    assert_eq!(
        facts[&412].title.as_str(),
        "Invoice list with filters",
        "the others stay"
    );
    // #415's kind is still asked, from its labels only
    let requests = received(&core).await;
    let (_, kinds) = requests.iter().find(|(p, _)| *p == Purpose::Kind).unwrap();
    let mine = kinds
        .iter()
        .find(|item| item["labels"] == json!(["export"]) && item["title"].is_null());
    assert!(mine.is_some(), "{kinds:?}");
    let after = serde_json::to_string(&requests[1..]).unwrap();
    assert!(!after.contains("Faster statement"), "{after}");
}

#[tokio::test]
async fn a_judge_that_isnt_jev_is_held_to_stricter_thresholds() {
    // a choice at 0.7 and a clean title at 0.4: enough for Jev (0.6, and under 0.5) ...
    let jev = core_with(JudgeMode::Jev, "never in a title", 0.7, 0.4).await;
    let mut live = judged_index(&fixture(), &jev).await;
    live.refresh().await.unwrap();
    let facts = by_number(&live).await;
    assert_eq!(facts[&422].status, Status::Blocked);
    assert!(!facts[&412].title_withheld);
    // ... not for a fallback, held 0.2 stricter (0.8, and from 0.3)
    let chat = core_with(JudgeMode::Chat, "never in a title", 0.7, 0.4).await;
    let mut live = judged_index(&fixture(), &chat).await;
    live.refresh().await.unwrap();
    let facts = by_number(&live).await;
    assert_eq!(facts[&422].status, Status::Unclear);
    assert!(facts[&412].title_withheld);
}

#[tokio::test]
async fn without_an_answer_a_conflict_stays_unclear_a_kind_unknown_and_the_titles_are_withheld() {
    let core = MockServer::start().await;
    // the core refuses the first round, then answers
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(3)
        .mount(&core)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core {
            mode: JudgeMode::Jev,
            flag: "never in a title",
            status_p: 0.81,
            kind_p: 0.9,
            clean_p: 0.02,
        })
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    assert!(live.refresh().await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert!(!report.complete());
    assert_eq!(report.error.unwrap().to_string(), "the core answered 503");
    let facts = by_number(&live).await;
    assert_eq!(facts[&422].status, Status::Unclear);
    assert!((facts[&422].confidence.get() - 0.5).abs() < 1e-9);
    assert_eq!(facts[&422].waiting_on, None);
    for number in KIND_OPEN {
        assert_eq!(facts[&number].kind, Kind::Unknown, "#{number}");
    }
    assert_eq!(facts[&422].kind, Kind::Fix, "a label's kind needs no judge");
    for (number, fact) in &facts {
        assert!(fact.title_withheld, "#{number}: no title is shown unjudged");
        assert!(fact.title.as_str().starts_with("Work item in "), "#{number}");
    }

    // nothing changed, but the questions are open: the next tick asks again
    assert!(
        live.refresh().await.unwrap().is_some(),
        "an incomplete round is built again"
    );
    assert!(live.judged().unwrap().complete());
    let facts = by_number(&live).await;
    assert_eq!(facts[&422].status, Status::Blocked);
    assert_eq!(facts[&412].title.as_str(), "Invoice list with filters");
    assert!(live.refresh().await.unwrap().is_none(), "then nothing more to do");
}

#[tokio::test]
async fn a_reply_that_isnt_the_requests_is_no_answer() {
    let core = MockServer::start().await;
    // the right purpose, the wrong ids
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "purpose": "title_safety", "judge_mode": "jev", "items": [{ "id": "someone_else", "p": 0.0 }]
        })))
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    live.refresh().await.unwrap();
    let report = live.judged().unwrap();
    assert!(!report.complete());
    assert_eq!(
        report.error.as_ref().unwrap().to_string(),
        "the core's reply doesn't answer the request"
    );
    assert!(by_number(&live).await.values().all(|fact| fact.title_withheld));
}

/// Now, as the probe's clock reads it.
fn now() -> DateTime<Utc> {
    DateTime::<Utc>::from(std::time::SystemTime::now())
}

/// The moment `after` `start`.
fn later(start: DateTime<Utc>, after: Duration) -> DateTime<Utc> {
    start + TimeDelta::from_std(after).unwrap()
}

#[tokio::test]
async fn a_title_only_the_rules_answered_is_withheld_until_a_judge_answers() {
    let core = MockServer::start().await;
    // no judge answers the first round, so the core's rules do: a hit in #415's title, nothing
    // found in the others (p 0)
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core::rules("Faster statement"))
        .up_to_n_times(3)
        .mount(&core)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core {
            mode: JudgeMode::Jev,
            flag: "never in a title",
            status_p: 0.81,
            kind_p: 0.9,
            clean_p: 0.02,
        })
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    let start = now();
    assert!(live.refresh_at(start).await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert_eq!(
        report.by_rules,
        19 - RULES_WITHHOLD.len(),
        "every title asked: {report:?}"
    );
    assert!(!report.failed(), "a gap, not a failure: {report:?}");
    assert!(!report.complete());
    let facts = by_number(&live).await;
    for (number, fact) in &facts {
        assert!(fact.title_withheld, "#{number}: p 0 from the rules shows nothing");
        assert!(fact.title.as_str().starts_with("Work item in "), "#{number}");
    }
    // the rules' "can't tell" settles no status and no kind
    assert_eq!(facts[&422].status, Status::Unclear);
    for number in KIND_OPEN {
        assert_eq!(facts[&number].kind, Kind::Unknown, "#{number}");
    }

    // nothing changed and only the rules answered: not asked again before five minutes
    for after in [REFRESH_EVERY, RULES_RECHECK - Duration::from_secs(1)] {
        assert!(
            live.refresh_at(later(start, after)).await.unwrap().is_none(),
            "{after:?}"
        );
    }
    assert_eq!(received(&core).await.len(), 3, "one round");

    // then a judge answers, below the line: the title shows
    assert!(
        live.refresh_at(later(start, RULES_RECHECK))
            .await
            .unwrap()
            .is_some()
    );
    assert!(live.judged().unwrap().complete());
    let facts = by_number(&live).await;
    assert_eq!(
        (facts[&412].title.as_str(), facts[&412].title_withheld),
        ("Invoice list with filters", false)
    );
    assert!(
        !facts[&415].title_withheld,
        "the rules' hit was asked again too: the judge's answer counts"
    );
    assert_eq!(facts[&422].status, Status::Blocked);
    assert!(
        live.refresh_at(later(start, RULES_RECHECK + REFRESH_EVERY))
            .await
            .unwrap()
            .is_none(),
        "then nothing more to do"
    );
}

#[tokio::test]
async fn a_failure_is_asked_again_at_the_next_tick_the_rules_titles_after_five_minutes() {
    let core = MockServer::start().await;
    // the first conflict's request gets no answer; every other request, the rules'
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .and(body_partial_json(json!({ "purpose": "status_from_candidates" })))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&core)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core::rules("never in a title"))
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    let start = now();
    assert!(live.refresh_at(start).await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert!(report.failed(), "{report:?}");
    assert_eq!(report.unanswered, 1);
    assert_eq!(report.by_rules, 19 - RULES_WITHHOLD.len());

    // a failure: the next tick asks again
    let tick = later(start, REFRESH_EVERY);
    assert!(live.refresh_at(tick).await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert!(!report.failed(), "{report:?}");
    assert_eq!(report.by_rules, 19 - RULES_WITHHOLD.len());

    // only the rules' titles are left: five minutes from that rebuild, not from the first
    for after in [REFRESH_EVERY, RULES_RECHECK - Duration::from_secs(1)] {
        assert!(
            live.refresh_at(later(tick, after)).await.unwrap().is_none(),
            "{after:?}"
        );
    }
    let again = later(tick, RULES_RECHECK);
    assert!(live.refresh_at(again).await.unwrap().is_some());
    assert_eq!(received(&core).await.len(), 9, "three rounds of three requests");
    assert!(
        by_number(&live).await.values().all(|fact| fact.title_withheld),
        "the rules still answer: every title withheld"
    );

    // a clock set back doesn't hold the titles back for longer
    assert!(
        live.refresh_at(again - TimeDelta::minutes(1))
            .await
            .unwrap()
            .is_some()
    );
}

/// A first request that the core answers `status` with the given headers, then the rules for the
/// rest, and the probe's first round on the fixture at `start`.
async fn told_to_wait(status: u16, headers: &[(&str, &str)]) -> (MockServer, LiveIndex, DateTime<Utc>) {
    let core = MockServer::start().await;
    let mut refused = ResponseTemplate::new(status);
    for (name, value) in headers {
        refused = refused.insert_header(*name, *value);
    }
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .and(body_partial_json(json!({ "purpose": "status_from_candidates" })))
        .respond_with(refused)
        .up_to_n_times(1)
        .mount(&core)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core {
            mode: JudgeMode::Jev,
            flag: "never in a title",
            status_p: 0.81,
            kind_p: 0.9,
            clean_p: 0.02,
        })
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    let start = now();
    assert!(live.refresh_at(start).await.unwrap().is_some());
    assert!(live.judged().unwrap().failed(), "{:?}", live.judged());
    (core, live, start)
}

#[tokio::test]
async fn a_core_that_says_when_to_come_back_is_not_asked_before() {
    let (core, mut live, start) = told_to_wait(429, &[("retry-after", "120")]).await;
    assert_eq!(live.judged().unwrap().retry_after, Some(Duration::from_secs(120)));
    let rounds = received(&core).await.len();

    // not at the next tick, nor a second before the time it named
    for after in [REFRESH_EVERY, Duration::from_secs(119)] {
        assert!(
            live.refresh_at(later(start, after)).await.unwrap().is_none(),
            "{after:?}"
        );
    }
    assert_eq!(
        received(&core).await.len(),
        rounds,
        "no request while it was told to wait"
    );

    // then asked again, and answered
    assert!(
        live.refresh_at(later(start, Duration::from_secs(120)))
            .await
            .unwrap()
            .is_some()
    );
    assert!(!live.judged().unwrap().failed());
    assert_eq!(live.judged().unwrap().retry_after, None);
    assert!(received(&core).await.len() > rounds);
}

#[tokio::test]
async fn a_wait_the_core_names_is_kept_to_ten_minutes_and_only_a_number_of_seconds_counts() {
    // a day is more than the probe will wait on a word
    let (_core, mut live, start) = told_to_wait(503, &[("retry-after", "86400")]).await;
    assert_eq!(
        live.judged().unwrap().retry_after,
        Some(Duration::from_secs(86_400))
    );
    let almost = MAX_RETRY_AFTER - Duration::from_secs(1);
    assert!(live.refresh_at(later(start, almost)).await.unwrap().is_none());
    assert!(
        live.refresh_at(later(start, MAX_RETRY_AFTER))
            .await
            .unwrap()
            .is_some()
    );

    // a date, or nothing: the next tick, as for any failure
    for header in [
        &[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")][..],
        &[("retry-after", "soon")][..],
        &[][..],
    ] {
        let (_core, mut live, start) = told_to_wait(503, header).await;
        assert_eq!(live.judged().unwrap().retry_after, None, "{header:?}");
        assert!(
            live.refresh_at(later(start, REFRESH_EVERY))
                .await
                .unwrap()
                .is_some(),
            "{header:?}"
        );
    }

    // a clock set back doesn't hold the question back for longer
    let (_core, mut live, start) = told_to_wait(429, &[("retry-after", "120")]).await;
    assert!(
        live.refresh_at(start - TimeDelta::minutes(1))
            .await
            .unwrap()
            .is_some()
    );
}

/// The core's rules can't tell a status or a kind, and say so with a low p. Even when a core that
/// answered by its rules gave one a high p, the probe settles nothing on it: a rules answer is no
/// answer to a status or a kind, whatever its number.
#[tokio::test]
async fn a_rules_answer_never_settles_a_status_or_a_kind() {
    let core = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/internal/judge"))
        .respond_with(Core {
            mode: JudgeMode::Rules,
            flag: "never in a title",
            status_p: 0.99,
            kind_p: 0.99,
            clean_p: 0.0,
        })
        .mount(&core)
        .await;
    let mut live = judged_index(&fixture(), &core).await;
    assert!(live.refresh_at(now()).await.unwrap().is_some());
    let facts = by_number(&live).await;
    assert_eq!(
        facts[&422].status,
        Status::Unclear,
        "a conflict the rules can't settle"
    );
    for number in KIND_OPEN {
        assert_eq!(facts[&number].kind, Kind::Unknown, "#{number}");
    }
    assert!(
        received(&core)
            .await
            .iter()
            .any(|(purpose, _)| *purpose == Purpose::StatusFromCandidates),
        "the questions were asked"
    );
}

#[test]
fn serve_needs_the_cores_address_and_dev_does_not() {
    let serve = Source::Serve {
        repo: PathBuf::from("/repo"),
        meta: PathBuf::from("/meta"),
        project: None,
    };
    let dev = Source::Dev {
        repo: fixture(),
        meta: fixture().join("meta"),
        token: None,
    };
    let unset = |_: &str| None;
    let blank = |_: &str| Some("  ".to_owned());
    let set = |name: &str| (name == "PROBE_CORE_INTERNAL_URL").then(|| "http://core:8081".to_owned());

    let refused = serve.core_address(unset).unwrap_err();
    assert!(
        refused.contains("PROBE_CORE_INTERNAL_URL is not set"),
        "{refused}"
    );
    assert!(
        serve.core_address(blank).is_err(),
        "an empty address is no address"
    );
    assert_eq!(
        serve.core_address(set).unwrap().as_deref(),
        Some("http://core:8081")
    );
    assert_eq!(
        dev.core_address(unset).unwrap(),
        None,
        "dev runs on the rules alone"
    );
    assert_eq!(dev.core_address(blank).unwrap(), None);
    assert_eq!(
        dev.core_address(set).unwrap().as_deref(),
        Some("http://core:8081")
    );
}

#[test]
fn a_serve_probe_without_the_cores_address_exits_with_a_message() {
    let ran = Command::new(env!("CARGO_BIN_EXE_mercury-probe"))
        .arg("serve")
        .env_remove("PROBE_CORE_INTERNAL_URL")
        .env_remove("PROBE_PROJECT")
        .env_remove("PROBE_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!ran.status.success());
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(
        said.contains("mercury-probe: PROBE_CORE_INTERNAL_URL is not set"),
        "{said}"
    );
}

#[tokio::test]
async fn the_cache_asks_once_per_item_while_it_stays_the_same() {
    let (_dir, repo) = own_fixture();
    let core = core(JudgeMode::Jev, "never in a title").await;
    let mut live = judged_index(&repo, &core).await;
    live.refresh().await.unwrap();
    let first = received(&core).await.len();
    assert_eq!(first, 3);

    // one issue's title changes: only it is asked again, its title and its open kind
    let github = repo.join("meta/github.json");
    let raw = fs::read_to_string(&github).unwrap();
    fs::write(
        &github,
        raw.replace("Tidy up the release checklist", "Tidy up the release notes"),
    )
    .unwrap();
    assert!(live.refresh().await.unwrap().is_some());
    let report = live.judged().unwrap().clone();
    assert_eq!((report.answered, report.requests), (2, 2), "{report:?}");
    let second: Vec<_> = received(&core).await.split_off(first);
    assert_eq!(count(&second, Purpose::TitleSafety), 1);
    assert_eq!(count(&second, Purpose::Kind), 1);
    assert_eq!(
        count(&second, Purpose::StatusFromCandidates),
        0,
        "#422 is the same: kept"
    );
    assert_eq!(
        report.cached,
        14 + 1 + 2,
        "the other titles, the conflict, the other kinds"
    );
    assert_eq!(
        by_number(&live).await[&432].title.as_str(),
        "Tidy up the release notes"
    );
}

#[tokio::test]
async fn a_fallbacks_answer_is_asked_again_at_the_next_rebuild() {
    let (_dir, repo) = own_fixture();
    let core = core(JudgeMode::Rules, "never in a title").await;
    let mut live = judged_index(&repo, &core).await;
    live.refresh().await.unwrap();
    let github = repo.join("meta/github.json");
    let raw = fs::read_to_string(&github).unwrap();
    fs::write(
        &github,
        raw.replace("Tidy up the release checklist", "Tidy up the notes"),
    )
    .unwrap();
    live.refresh().await.unwrap();
    assert_eq!(live.judged().unwrap().cached, 0, "nothing of the rules is kept");
    assert_eq!(received(&core).await.len(), 6);
}

#[tokio::test]
async fn only_the_texts_the_manifest_names_are_sent_redacted_with_its_rules() {
    let core = core(JudgeMode::Jev, "never in a title").await;
    let mut live = judged_index(&fixture(), &core).await;
    live.refresh().await.unwrap();
    let sent = serde_json::to_string(&received(&core).await).unwrap();
    assert!(!sent.contains("Megabank"), "the manifest's client name: {sent}");
    assert!(sent.contains("SEPA export for the client"));
    assert!(!sent.contains("billing@example.com"), "an e-mail address");
    for withheld in [
        "InvoiceNumberSequencer",
        "ledgerEntryCache",
        "SepaExportWriter",
        "files.example.com",
    ] {
        assert!(!sent.contains(withheld), "a title the rules withheld: {withheld}");
    }

    // a manifest that lets the judge read labels only
    let (_dir, repo) = own_fixture();
    let manifest = repo.join(".mercury/probe.yaml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("judge_context: [titles, labels]", "judge_context: [labels]");
    fs::write(&manifest, text).unwrap();
    let core = self::core(JudgeMode::Jev, "never in a title").await;
    let mut live = judged_index(&repo, &core).await;
    live.refresh().await.unwrap();
    let requests = received(&core).await;
    assert_eq!(
        count(&requests, Purpose::TitleSafety),
        0,
        "titles are the rules' alone"
    );
    assert!(
        !serde_json::to_string(&requests).unwrap().contains("\"title\":\""),
        "no title sent"
    );
    let facts = by_number(&live).await;
    assert_eq!(
        facts[&412].title.as_str(),
        "Invoice list with filters",
        "no title_safety, no withholding"
    );
    // #432 has no label and its title can't go: its kind isn't asked
    assert_eq!(count(&requests, Purpose::Kind), 2);
    assert_eq!(facts[&432].kind, Kind::Unknown);
}

#[tokio::test]
async fn without_a_judge_the_rules_alone_decide() {
    let source = Source::Dev {
        repo: fixture(),
        meta: fixture().join("meta"),
        token: None,
    };
    let mut live = LiveIndex::new(Index::in_memory().await.unwrap(), source);
    live.refresh().await.unwrap();
    assert!(live.judged().is_none());
    let facts = by_number(&live).await;
    assert_eq!(facts[&422].status, Status::Unclear);
    assert_eq!(facts[&415].kind, Kind::Unknown);
    assert_eq!(facts[&412].title.as_str(), "Invoice list with filters");
    assert!(live.refresh().await.unwrap().is_none());
}
