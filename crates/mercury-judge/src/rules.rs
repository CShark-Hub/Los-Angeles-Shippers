//! The rules judge, the last fallback: answers from patterns over the state's text, with no model
//! and no network. What the rules can't tell gets the unsafe end of the check.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use indexmap::IndexMap;
use mercury_schema::JudgeMode;
use regex::Regex;
use serde_json::Value;

use crate::{Answer, Batch, Judge, JudgeError, Outcome, Question};

/// Which end of a `noul` or a `score` is the unsafe one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// High is bad, as in "asks for code": the unsafe end is 1, the top of a scale.
    Risk,
    /// High is good, as in "the arguments match the question": the unsafe end is 0.
    Legit,
}

impl Polarity {
    fn unsafe_end(self) -> f32 {
        match self {
            Polarity::Risk => 1.0,
            Polarity::Legit => 0.0,
        }
    }
}

/// A test over a text, for what patterns can't express: the SafeText rules, a cited fact id.
#[derive(Clone)]
pub struct Test(pub Arc<dyn Fn(&str) -> bool + Send + Sync>);

impl Test {
    pub fn new(test: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(test))
    }
}

impl fmt::Debug for Test {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Test(..)")
    }
}

/// How the rules answer one question. `field` is a JSON pointer into the state, such as
/// `/question/text`.
#[derive(Debug, Clone)]
pub enum Rule {
    /// A `noul`: 1 when a pattern finds the text at `field`, 0 when none does. No text there, or
    /// no patterns, gets the unsafe end.
    Noul {
        polarity: Polarity,
        field: String,
        patterns: Vec<Regex>,
    },
    /// A `choice`: the one option whose patterns find the text at `field`. No text, no match or
    /// matches for several options is no clean match: every option equally probable, with no
    /// confidence.
    Choice {
        field: String,
        options: IndexMap<String, Vec<Regex>>,
    },
    /// A `noul`: 1 when the test holds for the text at `field`, 0 when it doesn't. No text there
    /// gets the unsafe end.
    Predicate {
        polarity: Polarity,
        field: String,
        test: Test,
    },
    /// A question the rules can't decide: a `noul` or a `score` gets its unsafe end, a `choice`
    /// no clean match.
    Undecided { polarity: Polarity },
}

/// Answers by rule, keyed by question id. A question without a rule, or with a rule for another
/// type of question, is a configuration error, never a guess.
#[derive(Debug, Clone, Default)]
pub struct RulesJudge {
    rules: HashMap<String, Rule>,
}

impl RulesJudge {
    pub fn new(rules: HashMap<String, Rule>) -> Self {
        Self { rules }
    }
}

#[async_trait]
impl Judge for RulesJudge {
    async fn decide(&self, batch: &Batch) -> Result<Outcome, JudgeError> {
        let started = Instant::now();
        batch.validate()?;
        let mut answers = IndexMap::with_capacity(batch.questions.len());
        for (id, question) in &batch.questions {
            let rule = self.rules.get(id).ok_or_else(|| JudgeError::NoRule(id.clone()))?;
            let answer =
                answer(question, rule, &batch.state).ok_or_else(|| JudgeError::RuleMismatch(id.clone()))?;
            answers.insert(id.clone(), answer);
        }
        Ok(Outcome {
            answers,
            mode: JudgeMode::Rules,
            cost_usd: Some(0.0),
            latency_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            request_id: None,
            model: None,
            prompt_version: None,
        })
    }
}

/// `None` when the rule is for another type of question.
fn answer(question: &Question, rule: &Rule, state: &Value) -> Option<Answer> {
    match (question, rule) {
        (
            Question::Noul { .. },
            Rule::Noul {
                polarity,
                field,
                patterns,
            },
        ) => Some(Answer::Noul(match text(state, field) {
            Some(text) if !patterns.is_empty() => {
                if patterns.iter().any(|pattern| pattern.is_match(text)) {
                    1.0
                } else {
                    0.0
                }
            }
            _ => polarity.unsafe_end(),
        })),
        (
            Question::Noul { .. },
            Rule::Predicate {
                polarity,
                field,
                test,
            },
        ) => Some(Answer::Noul(match text(state, field) {
            Some(text) => {
                if (test.0)(text) {
                    1.0
                } else {
                    0.0
                }
            }
            None => polarity.unsafe_end(),
        })),
        (Question::Noul { .. }, Rule::Undecided { polarity }) => Some(Answer::Noul(polarity.unsafe_end())),
        (Question::Choice { criteria, .. }, Rule::Choice { field, options }) => Some(
            match text(state, field).and_then(|text| clean_match(criteria, options, text)) {
                Some(option) => certain(criteria, option),
                None => unsure(criteria),
            },
        ),
        (Question::Choice { criteria, .. }, Rule::Undecided { .. }) => Some(unsure(criteria)),
        (Question::Score { criteria, .. }, Rule::Undecided { polarity }) => {
            let level = match polarity {
                Polarity::Risk => criteria.len() - 1,
                Polarity::Legit => 0,
            };
            Some(Answer::Score {
                score: polarity.unsafe_end(),
                probabilities: (0..criteria.len())
                    .map(|l| (l.to_string(), if l == level { 1.0 } else { 0.0 }))
                    .collect(),
                confidence: 0.0,
            })
        }
        _ => None,
    }
}

fn text<'a>(state: &'a Value, field: &str) -> Option<&'a str> {
    state.pointer(field).and_then(Value::as_str)
}

/// The question's one option that a pattern finds in `text`, if exactly one is found.
fn clean_match<'a>(
    criteria: &'a IndexMap<String, Value>,
    options: &IndexMap<String, Vec<Regex>>,
    text: &str,
) -> Option<&'a str> {
    let mut found = criteria.keys().filter(|option| {
        options
            .get(*option)
            .is_some_and(|patterns| patterns.iter().any(|pattern| pattern.is_match(text)))
    });
    match (found.next(), found.next()) {
        (Some(option), None) => Some(option),
        _ => None,
    }
}

fn certain(criteria: &IndexMap<String, Value>, picked: &str) -> Answer {
    Answer::Choice {
        choice: picked.to_owned(),
        probabilities: criteria
            .keys()
            .map(|option| (option.clone(), if option == picked { 1.0 } else { 0.0 }))
            .collect(),
        confidence: 1.0,
    }
}

/// Every option equally probable, so with two options or more none is above 0.5.
fn unsure(criteria: &IndexMap<String, Value>) -> Answer {
    let p = 1.0 / criteria.len() as f32;
    Answer::Choice {
        // validate() keeps a choice at two options or more
        choice: criteria.keys().next().cloned().unwrap_or_default(),
        probabilities: criteria.keys().map(|option| (option.clone(), p)).collect(),
        confidence: 0.0,
    }
}
