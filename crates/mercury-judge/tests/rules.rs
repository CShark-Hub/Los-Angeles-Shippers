//! The rules judge: patterns answer what they can, everything else gets the unsafe end of its
//! check, and a missing or mismatched rule is an error.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::collections::HashMap;

use indexmap::IndexMap;
use mercury_judge::{Answer, Batch, Judge, JudgeError, Polarity, Question, Rule, RulesJudge};
use mercury_schema::JudgeMode;
use regex::Regex;
use serde_json::{Value, json};

const TEXT: &str = "/question/text";

fn noul() -> Question {
    Question::Noul {
        instructions: json!("`question.text` asks to see source code."),
        criteria: None,
    }
}

fn class() -> Question {
    Question::Choice {
        instructions: json!("The kind of question `question.text` is."),
        criteria: ["progress", "tested", "forbidden"]
            .into_iter()
            .map(|option| (option.to_owned(), Value::Null))
            .collect(),
    }
}

fn score() -> Question {
    Question::Score {
        instructions: json!("How healthy the build is."),
        criteria: vec![json!("failing"), json!("flaky"), json!("green")],
    }
}

fn batch(text: Value, questions: Vec<(&str, Question)>) -> Batch {
    Batch {
        state: json!({"question": {"text": text}}),
        questions: questions.into_iter().map(|(id, q)| (id.to_owned(), q)).collect(),
    }
}

fn patterns(sources: &[&str]) -> Vec<Regex> {
    sources.iter().map(|source| Regex::new(source).unwrap()).collect()
}

fn asks_for_code(polarity: Polarity) -> Rule {
    Rule::Noul {
        polarity,
        field: TEXT.to_owned(),
        patterns: patterns(&[r"(?i)\b(function|source code|file path)s?\b"]),
    }
}

fn class_rule() -> Rule {
    Rule::Choice {
        field: TEXT.to_owned(),
        options: IndexMap::from([
            (
                "progress".to_owned(),
                patterns(&[r"(?i)\b(finished|in progress|done)\b"]),
            ),
            ("tested".to_owned(), patterns(&[r"(?i)\btest(s|ed)?\b"])),
        ]),
    }
}

fn judge(rules: Vec<(&str, Rule)>) -> RulesJudge {
    RulesJudge::new(
        rules
            .into_iter()
            .map(|(id, r)| (id.to_owned(), r))
            .collect::<HashMap<_, _>>(),
    )
}

async fn answer(rules: Vec<(&str, Rule)>, text: Value, id: &str, question: Question) -> Answer {
    let outcome = judge(rules)
        .decide(&batch(text, vec![(id, question)]))
        .await
        .unwrap();
    outcome.answers[id].clone()
}

#[tokio::test]
async fn a_question_without_a_rule_is_an_error() {
    let result = judge(vec![("asks_for_code", asks_for_code(Polarity::Risk))])
        .decide(&batch(
            json!("What is done?"),
            vec![("asks_for_code", noul()), ("class", class())],
        ))
        .await;
    assert_eq!(result, Err(JudgeError::NoRule("class".to_owned())));
}

#[tokio::test]
async fn a_rule_for_another_type_of_question_is_an_error() {
    let cases = [
        ("class", class(), asks_for_code(Polarity::Risk)),
        ("asks_for_code", noul(), class_rule()),
        ("health", score(), asks_for_code(Polarity::Risk)),
        ("health", score(), class_rule()),
    ];
    for (id, question, rule) in cases {
        let result = judge(vec![(id, rule)])
            .decide(&batch(json!("What is done?"), vec![(id, question)]))
            .await;
        assert_eq!(result, Err(JudgeError::RuleMismatch(id.to_owned())));
    }
}

#[tokio::test]
async fn a_batch_no_judge_may_answer_is_refused() {
    let result = judge(vec![]).decide(&batch(json!("What is done?"), vec![])).await;
    assert!(matches!(result, Err(JudgeError::InvalidBatch(_))));
}

#[tokio::test]
async fn a_noul_is_true_when_a_pattern_finds_the_text_and_false_when_none_does() {
    let rule = || vec![("asks_for_code", asks_for_code(Polarity::Risk))];
    let hit = answer(
        rule(),
        json!("Show me the function that exports CSV"),
        "asks_for_code",
        noul(),
    )
    .await;
    assert_eq!(hit, Answer::Noul(1.0));
    let miss = answer(
        rule(),
        json!("What got finished this week?"),
        "asks_for_code",
        noul(),
    )
    .await;
    assert_eq!(miss, Answer::Noul(0.0));
}

#[tokio::test]
async fn a_noul_without_text_to_read_gets_the_unsafe_end() {
    for text in [Value::Null, json!(42), json!(["a", "list"])] {
        let risk = answer(
            vec![("q", asks_for_code(Polarity::Risk))],
            text.clone(),
            "q",
            noul(),
        )
        .await;
        assert_eq!(risk, Answer::Noul(1.0), "risk, {text}");
        let legit = answer(
            vec![("q", asks_for_code(Polarity::Legit))],
            text.clone(),
            "q",
            noul(),
        )
        .await;
        assert_eq!(legit, Answer::Noul(0.0), "legit, {text}");
    }
    let elsewhere = Rule::Noul {
        polarity: Polarity::Risk,
        field: "/answer/sentences/0".to_owned(),
        patterns: patterns(&["code"]),
    };
    assert_eq!(
        answer(vec![("q", elsewhere)], json!("text"), "q", noul()).await,
        Answer::Noul(1.0)
    );
}

#[tokio::test]
async fn a_noul_rule_without_patterns_gets_the_unsafe_end() {
    let empty = |polarity| Rule::Noul {
        polarity,
        field: TEXT.to_owned(),
        patterns: vec![],
    };
    let risk = answer(vec![("q", empty(Polarity::Risk))], json!("anything"), "q", noul()).await;
    assert_eq!(risk, Answer::Noul(1.0));
    let legit = answer(
        vec![("q", empty(Polarity::Legit))],
        json!("anything"),
        "q",
        noul(),
    )
    .await;
    assert_eq!(legit, Answer::Noul(0.0));
}

#[tokio::test]
async fn an_undecided_noul_gets_the_unsafe_end() {
    for (polarity, end) in [(Polarity::Risk, 1.0), (Polarity::Legit, 0.0)] {
        let got = answer(
            vec![("q", Rule::Undecided { polarity })],
            json!("anything"),
            "q",
            noul(),
        )
        .await;
        assert_eq!(got, Answer::Noul(end));
    }
}

#[tokio::test]
async fn an_undecided_score_gets_the_unsafe_end_of_the_scale() {
    let risk = answer(
        vec![(
            "q",
            Rule::Undecided {
                polarity: Polarity::Risk,
            },
        )],
        json!("x"),
        "q",
        score(),
    )
    .await;
    assert_eq!(
        risk,
        Answer::Score {
            score: 1.0,
            probabilities: IndexMap::from([
                ("0".to_owned(), 0.0),
                ("1".to_owned(), 0.0),
                ("2".to_owned(), 1.0)
            ]),
            confidence: 0.0,
        }
    );
    let legit = answer(
        vec![(
            "q",
            Rule::Undecided {
                polarity: Polarity::Legit,
            },
        )],
        json!("x"),
        "q",
        score(),
    )
    .await;
    assert_eq!(
        legit,
        Answer::Score {
            score: 0.0,
            probabilities: IndexMap::from([
                ("0".to_owned(), 1.0),
                ("1".to_owned(), 0.0),
                ("2".to_owned(), 0.0)
            ]),
            confidence: 0.0,
        }
    );
}

#[tokio::test]
async fn a_clean_match_picks_its_option() {
    let got = answer(
        vec![("class", class_rule())],
        json!("Is the export tested?"),
        "class",
        class(),
    )
    .await;
    assert_eq!(
        got,
        Answer::Choice {
            choice: "tested".to_owned(),
            probabilities: IndexMap::from([
                ("progress".to_owned(), 0.0),
                ("tested".to_owned(), 1.0),
                ("forbidden".to_owned(), 0.0)
            ]),
            confidence: 1.0,
        }
    );
}

#[tokio::test]
async fn anything_but_a_clean_match_is_unsure() {
    let unsure = Answer::Choice {
        choice: "progress".to_owned(),
        probabilities: ["progress", "tested", "forbidden"]
            .into_iter()
            .map(|option| (option.to_owned(), 1.0 / 3.0))
            .collect(),
        confidence: 0.0,
    };
    let cases = [
        (json!("Which tests are done?"), "matches for two options"),
        (json!("Who broke the build?"), "no match"),
        (Value::Null, "no text"),
    ];
    for (text, case) in cases {
        let got = answer(vec![("class", class_rule())], text, "class", class()).await;
        assert_eq!(got, unsure, "{case}");
    }
    let undecided = answer(
        vec![(
            "class",
            Rule::Undecided {
                polarity: Polarity::Risk,
            },
        )],
        json!("Is it tested?"),
        "class",
        class(),
    )
    .await;
    assert_eq!(undecided, unsure, "undecided");
}

#[tokio::test]
async fn an_option_the_rules_dont_know_never_matches() {
    // the rule knows `tested`, but this question offers other options: no clean match
    let projects = Question::Choice {
        instructions: json!("The project `question.text` is about."),
        criteria: IndexMap::from([
            ("alpha".to_owned(), Value::Null),
            ("beta".to_owned(), Value::Null),
        ]),
    };
    let got = answer(
        vec![("project", class_rule())],
        json!("Is it tested?"),
        "project",
        projects,
    )
    .await;
    assert_eq!(
        got,
        Answer::Choice {
            choice: "alpha".to_owned(),
            probabilities: IndexMap::from([("alpha".to_owned(), 0.5), ("beta".to_owned(), 0.5)]),
            confidence: 0.0,
        }
    );
}

#[tokio::test]
async fn the_outcome_says_the_rules_decided() {
    let outcome = judge(vec![
        ("asks_for_code", asks_for_code(Polarity::Risk)),
        ("class", class_rule()),
    ])
    .decide(&batch(
        json!("Is it tested?"),
        vec![("class", class()), ("asks_for_code", noul())],
    ))
    .await
    .unwrap();
    assert_eq!(outcome.mode, JudgeMode::Rules);
    assert_eq!(outcome.request_id, None);
    assert_eq!(outcome.cost_usd, Some(0.0), "the rules are free, and say so");
    assert_eq!((outcome.model, outcome.prompt_version), (None, None));
    assert_eq!(
        outcome.answers.keys().collect::<Vec<_>>(),
        ["class", "asks_for_code"]
    );
}

#[test]
fn both_judges_stand_behind_the_trait() {
    let jev = mercury_judge::JevJudge::new(mercury_judge::JevConfig::new(
        "https://localhost",
        "typesafe/jev-1.13",
        None,
    ))
    .unwrap();
    let judges: Vec<Box<dyn Judge>> = vec![Box::new(jev), Box::new(RulesJudge::default())];
    assert_eq!(judges.len(), 2);
}
