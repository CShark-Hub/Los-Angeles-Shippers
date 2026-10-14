//! What a judge is asked and what it answers, in the shapes of Jev's Decisions API.

use indexmap::IndexMap;
use mercury_schema::JudgeMode;
use serde::Serialize;
use serde_json::Value;

use crate::JudgeError;

/// The fewest options a `choice` may offer: one option is no choice, so code decides it.
pub const MIN_OPTIONS: usize = 2;
/// The most options a `choice` may offer.
pub const MAX_OPTIONS: usize = 255;
/// The fewest levels a `score` may have.
pub const MIN_LEVELS: usize = 2;
/// The most levels a `score` may have.
pub const MAX_LEVELS: usize = 10;

/// One question about the state. `instructions` and every description may be a string, an object
/// or an array; the judge never sees the question's id.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// How probable it is that a statement about the state is true.
    Noul {
        instructions: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Which option fits the state best. Each option maps to its description, or `null`.
    Choice {
        instructions: Value,
        criteria: IndexMap<String, Value>,
    },
    /// Where the state sits on a scale, its levels described from the lowest to the highest.
    Score {
        instructions: Value,
        criteria: Vec<Value>,
    },
}

/// What makes a `noul` statement true, and what makes it false.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub when_true: Value,
    #[serde(rename = "false")]
    pub when_false: Value,
}

/// Every question of one stage, about one state, keyed by ids the caller chooses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Batch {
    /// A string, an object or an array.
    pub state: Value,
    pub questions: IndexMap<String, Question>,
}

impl Batch {
    /// Refuses a batch no judge may answer, before anything is sent.
    pub(crate) fn validate(&self) -> Result<(), JudgeError> {
        if self.questions.is_empty() {
            return Err(JudgeError::InvalidBatch("it has no questions".to_owned()));
        }
        if !(self.state.is_string() || self.state.is_object() || self.state.is_array()) {
            return Err(JudgeError::InvalidBatch(
                "the state is not a string, an object or an array".to_owned(),
            ));
        }
        for (id, question) in &self.questions {
            match question {
                Question::Noul { .. } => {}
                Question::Choice { criteria, .. } => {
                    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&criteria.len()) {
                        return Err(JudgeError::InvalidBatch(format!(
                            "`{id}` needs {MIN_OPTIONS} to {MAX_OPTIONS} options"
                        )));
                    }
                }
                Question::Score { criteria, .. } => {
                    if !(MIN_LEVELS..=MAX_LEVELS).contains(&criteria.len()) {
                        return Err(JudgeError::InvalidBatch(format!(
                            "`{id}` needs {MIN_LEVELS} to {MAX_LEVELS} levels"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// An answer, checked against its question: every probability and confidence is within 0–1.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The probability that the statement is true.
    Noul(f32),
    /// The most probable option; every option's probability, summing to 1; how sure the judge is.
    Choice {
        choice: String,
        probabilities: IndexMap<String, f32>,
        confidence: f32,
    },
    /// The position on the scale, 0 at the lowest level and 1 at the highest; each level's
    /// probability, keyed `"0"`, `"1"`, … from the lowest; how sure the judge is.
    Score {
        score: f32,
        probabilities: IndexMap<String, f32>,
        confidence: f32,
    },
}

/// A batch decided: one answer per question, in the batch's order, and which judge gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub answers: IndexMap<String, Answer>,
    pub mode: JudgeMode,
    /// What the request cost, as the judge reported it. `None` when it reported nothing, so a
    /// budget can tell an unknown cost from a free one; the rules cost `Some(0.0)`.
    pub cost_usd: Option<f64>,
    pub latency_ms: u32,
    /// The judge's id for the request, when it gives one.
    pub request_id: Option<String>,
    /// The model that answered, as the judge names it (Jev's dated version, such as
    /// `typesafe/jev-1.13-20260917`); `None` for the rules.
    pub model: Option<String>,
    /// The version of the prompt a chat judge answered from, such as `judge_fallback.v1`.
    pub prompt_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn batch(id: &str, question: Question) -> Batch {
        Batch {
            state: json!({"question": {"text": "What is blocked?"}}),
            questions: IndexMap::from([(id.to_owned(), question)]),
        }
    }

    fn choice(options: usize) -> Question {
        Question::Choice {
            instructions: json!("Which class is it?"),
            criteria: (0..options).map(|i| (format!("o{i}"), Value::Null)).collect(),
        }
    }

    fn score(levels: usize) -> Question {
        Question::Score {
            instructions: json!("How healthy is the build?"),
            criteria: (0..levels).map(|i| json!(format!("level {i}"))).collect(),
        }
    }

    #[test]
    fn questions_travel_in_the_decisions_shape() {
        let noul = Question::Noul {
            instructions: json!("`question.text` asks for code."),
            criteria: Some(NoulCriteria {
                when_true: json!("it names a function"),
                when_false: json!("it asks about progress"),
            }),
        };
        let bare = Question::Noul {
            instructions: json!({"statement": "a structured instruction"}),
            criteria: None,
        };
        let options = Question::Choice {
            instructions: json!("Which project?"),
            criteria: IndexMap::from([
                ("alpha".to_owned(), Value::Null),
                ("beta".to_owned(), json!("the other one")),
            ]),
        };
        assert_eq!(
            serde_json::to_value([noul, bare, options, score(2)]).unwrap(),
            json!([
                {"type": "noul", "instructions": "`question.text` asks for code.",
                 "criteria": {"true": "it names a function", "false": "it asks about progress"}},
                {"type": "noul", "instructions": {"statement": "a structured instruction"}},
                {"type": "choice", "instructions": "Which project?", "criteria": {"alpha": null, "beta": "the other one"}},
                {"type": "score", "instructions": "How healthy is the build?", "criteria": ["level 0", "level 1"]}
            ])
        );
    }

    #[test]
    fn a_batch_without_questions_is_refused() {
        let empty = Batch {
            state: json!({}),
            questions: IndexMap::new(),
        };
        assert!(matches!(empty.validate(), Err(JudgeError::InvalidBatch(_))));
    }

    #[test]
    fn a_state_must_be_a_string_an_object_or_an_array() {
        for state in [json!(null), json!(true), json!(3)] {
            let mut bad = batch("q", score(2));
            bad.state = state;
            assert!(matches!(bad.validate(), Err(JudgeError::InvalidBatch(_))));
        }
        for state in [json!("text"), json!({}), json!([])] {
            let mut good = batch("q", score(2));
            good.state = state;
            assert_eq!(good.validate(), Ok(()));
        }
    }

    #[test]
    fn a_choice_has_two_to_255_options() {
        for options in [0, 1, MAX_OPTIONS + 1] {
            assert!(matches!(
                batch("q", choice(options)).validate(),
                Err(JudgeError::InvalidBatch(_))
            ));
        }
        for options in [MIN_OPTIONS, MAX_OPTIONS] {
            assert_eq!(batch("q", choice(options)).validate(), Ok(()));
        }
    }

    #[test]
    fn a_score_has_two_to_ten_levels() {
        for levels in [0, 1, MAX_LEVELS + 1] {
            assert!(matches!(
                batch("q", score(levels)).validate(),
                Err(JudgeError::InvalidBatch(_))
            ));
        }
        for levels in [MIN_LEVELS, MAX_LEVELS] {
            assert_eq!(batch("q", score(levels)).validate(), Ok(()));
        }
    }
}
