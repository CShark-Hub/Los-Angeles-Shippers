//! The chain Jev → chat judge → rules. Every failure ends in a lower judge's outcome, whose mode
//! says which judge answered, or in an error when the rules fail too: never in Jev's mode, never
//! in a default. The breaker skips Jev for 60 s after three failures in 30 s.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use mercury_judge::{
    Answer, ApiKey, Batch, Clock, JevConfig, JevJudge, Judge, JudgeError, Outcome, Polarity, Question,
    ResilientJudge, Rule, RulesJudge,
};
use mercury_schema::JudgeMode;
use regex::Regex;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn batch() -> Batch {
    Batch {
        state: json!({"question": {"text": "Show me the export code", "role": "pm", "project": "delta"}}),
        questions: IndexMap::from([(
            "asks_for_code".to_owned(),
            Question::Noul {
                instructions: json!("`question.text` asks to see source code or file paths."),
                criteria: None,
            },
        )]),
    }
}

fn outcome(mode: JudgeMode) -> Outcome {
    Outcome {
        answers: IndexMap::from([("asks_for_code".to_owned(), Answer::Noul(0.97))]),
        mode,
        cost_usd: Some(0.0),
        latency_ms: 1,
        request_id: None,
        model: None,
        prompt_version: None,
    }
}

/// A judge that gives the same result every time and counts how often it was asked.
struct Scripted {
    result: Result<Outcome, JudgeError>,
    asked: Arc<AtomicUsize>,
}

#[async_trait]
impl Judge for Scripted {
    async fn decide(&self, _batch: &Batch) -> Result<Outcome, JudgeError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.result.clone()
    }
}

/// A scripted judge, and how often it was asked.
fn scripted(result: Result<Outcome, JudgeError>) -> (Box<dyn Judge>, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let judge = Scripted {
        result,
        asked: asked.clone(),
    };
    (Box::new(judge), asked)
}

fn asked(count: &AtomicUsize) -> usize {
    count.load(Ordering::SeqCst)
}

/// A clock the test turns by hand.
fn hand_clock() -> (Clock, Arc<Mutex<Instant>>) {
    let now = Arc::new(Mutex::new(Instant::now()));
    let read = now.clone();
    (Box::new(move || *read.lock().unwrap()), now)
}

fn advance(now: &Mutex<Instant>, by: Duration) {
    *now.lock().unwrap() += by;
}

#[tokio::test]
async fn jev_answers_when_it_can() {
    let (jev, jev_asked) = scripted(Ok(outcome(JudgeMode::Jev)));
    let (chat, chat_asked) = scripted(Ok(outcome(JudgeMode::Chat)));
    let (rules, rules_asked) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Jev);
    assert_eq!(
        (asked(&jev_asked), asked(&chat_asked), asked(&rules_asked)),
        (1, 0, 0)
    );
}

#[tokio::test]
async fn a_jev_failure_falls_to_the_chat_judge_in_the_same_request() {
    let (jev, jev_asked) = scripted(Err(JudgeError::Status(503)));
    let (chat, chat_asked) = scripted(Ok(outcome(JudgeMode::Chat)));
    let (rules, rules_asked) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Chat);
    assert_eq!(
        (asked(&jev_asked), asked(&chat_asked), asked(&rules_asked)),
        (1, 1, 0)
    );
}

#[tokio::test]
async fn with_jev_and_the_chat_judge_failing_the_rules_answer_in_the_same_request() {
    let (jev, _) = scripted(Err(JudgeError::Timeout(2000)));
    let (chat, _) = scripted(Err(JudgeError::Status(500)));
    let (rules, rules_asked) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Rules);
    assert_eq!(asked(&rules_asked), 1);
}

#[tokio::test]
async fn every_failure_ends_in_a_lower_mode_or_an_error_never_in_jevs() {
    let failures = [
        JudgeError::NoKey,
        JudgeError::InsecureUrl,
        JudgeError::Setup,
        JudgeError::Timeout(2000),
        JudgeError::Transport,
        JudgeError::Status(429),
        JudgeError::Malformed("not json".to_owned()),
        JudgeError::MissingAnswer("asks_for_code".to_owned()),
        JudgeError::UnexpectedAnswer,
        JudgeError::InvalidAnswer {
            id: "asks_for_code".to_owned(),
            reason: "the noul is outside 0–1",
        },
    ];
    for failure in failures {
        let (jev, _) = scripted(Err(failure.clone()));
        let (chat, _) = scripted(Err(failure.clone()));
        let (rules, _) = scripted(Ok(outcome(JudgeMode::Rules)));
        let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
        let mode = chain.decide(&batch()).await.unwrap().mode;
        assert_eq!(mode, JudgeMode::Rules, "{failure}");

        let (jev, _) = scripted(Err(failure.clone()));
        let (chat, _) = scripted(Err(failure.clone()));
        let (rules, _) = scripted(Err(JudgeError::NoRule("asks_for_code".to_owned())));
        let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
        assert_eq!(
            chain.decide(&batch()).await,
            Err(JudgeError::NoRule("asks_for_code".to_owned())),
            "{failure}: the last judge's error stays an error"
        );
    }
}

#[tokio::test]
async fn three_jev_failures_in_thirty_seconds_skip_it_for_sixty() {
    let (jev, jev_asked) = scripted(Err(JudgeError::Status(502)));
    let (chat, chat_asked) = scripted(Ok(outcome(JudgeMode::Chat)));
    let (rules, _) = scripted(Ok(outcome(JudgeMode::Rules)));
    let (clock, now) = hand_clock();
    let chain = ResilientJudge::new(Some(jev), Some(chat), rules).with_clock(clock);

    for _ in 0..3 {
        assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Chat);
        advance(&now, Duration::from_secs(10));
    }
    assert_eq!(asked(&jev_asked), 3);
    assert!(chain.skips_jev(), "three failures within 30 s");

    // 59 s after the third failure, Jev is still skipped; the chat judge answers at once
    advance(&now, Duration::from_secs(49));
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Chat);
    assert_eq!(asked(&jev_asked), 3, "not asked while the breaker is open");

    advance(&now, Duration::from_secs(1));
    assert!(!chain.skips_jev(), "60 s on");
    chain.decide(&batch()).await.unwrap();
    assert_eq!(asked(&jev_asked), 4, "asked again");
    assert_eq!(asked(&chat_asked), 5);
}

#[tokio::test]
async fn without_jev_the_chain_starts_at_the_chat_judge() {
    let (chat, _) = scripted(Ok(outcome(JudgeMode::Chat)));
    let (rules, _) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(None, Some(chat), rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Chat);

    let (chat, _) = scripted(Err(JudgeError::Status(503)));
    let (rules, _) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(None, Some(chat), rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Rules);
}

#[tokio::test]
async fn without_a_chat_judge_jev_drops_straight_to_the_rules() {
    let (jev, _) = scripted(Err(JudgeError::Transport));
    let (rules, _) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(Some(jev), None, rules);
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Rules);
}

#[tokio::test]
async fn a_batch_no_judge_may_answer_is_refused_and_counts_against_no_one() {
    let (jev, jev_asked) = scripted(Ok(outcome(JudgeMode::Jev)));
    let (chat, chat_asked) = scripted(Ok(outcome(JudgeMode::Chat)));
    let (rules, rules_asked) = scripted(Ok(outcome(JudgeMode::Rules)));
    let chain = ResilientJudge::new(Some(jev), Some(chat), rules);
    let mut empty = batch();
    empty.questions.clear();
    for _ in 0..3 {
        assert!(matches!(
            chain.decide(&empty).await,
            Err(JudgeError::InvalidBatch(_))
        ));
    }
    assert_eq!(
        (asked(&jev_asked), asked(&chat_asked), asked(&rules_asked)),
        (0, 0, 0)
    );
    assert!(!chain.skips_jev());
    assert_eq!(chain.decide(&batch()).await.unwrap().mode, JudgeMode::Jev);
}

/// The rules for [`batch`]'s one question: asking for code is a pattern on the question's text.
fn rules() -> RulesJudge {
    RulesJudge::new(HashMap::from([(
        "asks_for_code".to_owned(),
        Rule::Noul {
            polarity: Polarity::Risk,
            field: "/question/text".to_owned(),
            patterns: vec![Regex::new(r"(?i)\bcode\b").unwrap()],
        },
    )]))
}

#[tokio::test]
async fn with_the_decisions_endpoint_down_the_rules_answer_and_the_breaker_spares_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    let mut config = JevConfig::new(
        format!("{}/api/alpha/decisions", server.uri()),
        "typesafe/jev-1.13",
        ApiKey::new("not-a-real-key"),
    );
    config.https_only = false;
    let jev = JevJudge::new(config).unwrap();
    let chain = ResilientJudge::new(Some(Box::new(jev)), None, Box::new(rules()));

    for request in 1..=5 {
        let outcome = chain.decide(&batch()).await.unwrap();
        assert_eq!(outcome.mode, JudgeMode::Rules, "request {request}");
        assert_eq!(
            outcome.answers["asks_for_code"],
            Answer::Noul(1.0),
            "a pattern hit"
        );
    }
    // three requests reached the endpoint; the breaker kept the other two away
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}
