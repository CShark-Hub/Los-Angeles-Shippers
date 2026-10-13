//! The strict parse of a judge's answers, shared by every judge that answers in the Decisions
//! shapes: Jev, and the chat judge that imitates it.

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use crate::{Answer, Batch, JudgeError, Question};

/// How far the probabilities of one answer may sum from 1, for the judge's rounding.
const SUM_TOLERANCE: f64 = 0.02;

/// How much more probable than the pick another option may be and still count as tied with it:
/// the rounding of one value to `f32`, not a difference the judge meant.
const PICK_TOLERANCE: f64 = 1e-6;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
}

/// Every question answered once, and nothing else answered.
pub(crate) fn check_answers(
    batch: &Batch,
    mut answers: IndexMap<String, Value>,
) -> Result<IndexMap<String, Answer>, JudgeError> {
    let mut checked = IndexMap::with_capacity(batch.questions.len());
    for (id, question) in &batch.questions {
        let answer = answers
            .swap_remove(id)
            .ok_or_else(|| JudgeError::MissingAnswer(id.clone()))?;
        let answer = check(question, answer).map_err(|reason| JudgeError::InvalidAnswer {
            id: id.clone(),
            reason,
        })?;
        checked.insert(id.clone(), answer);
    }
    if !answers.is_empty() {
        return Err(JudgeError::UnexpectedAnswer);
    }
    Ok(checked)
}

fn check(question: &Question, answer: Value) -> Result<Answer, &'static str> {
    let answer: WireAnswer =
        serde_json::from_value(answer).map_err(|_| "not a noul, choice or score answer with its fields")?;
    match (question, answer) {
        (Question::Noul { .. }, WireAnswer::Noul { noul }) => {
            Ok(Answer::Noul(unit(noul, "the noul is outside 0–1")?))
        }
        (
            Question::Choice { criteria, .. },
            WireAnswer::Choice {
                choice,
                probabilities,
                confidence,
            },
        ) => {
            let probabilities = distribution(
                &probabilities,
                criteria.keys().map(String::as_str),
                "the probabilities are not keyed by the options",
            )?;
            let picked = *probabilities
                .get(&choice)
                .ok_or("the choice is not one of the options")?;
            if probabilities
                .values()
                .any(|&p| f64::from(p) > f64::from(picked) + PICK_TOLERANCE)
            {
                return Err("the choice is not the most probable option");
            }
            Ok(Answer::Choice {
                choice,
                probabilities,
                confidence: unit(confidence, "the confidence is outside 0–1")?,
            })
        }
        (
            Question::Score { criteria, .. },
            WireAnswer::Score {
                score,
                probabilities,
                confidence,
            },
        ) => {
            let levels: Vec<String> = (0..criteria.len()).map(|level| level.to_string()).collect();
            let probabilities = distribution(
                &probabilities,
                levels.iter().map(String::as_str),
                "the probabilities are not keyed by the levels",
            )?;
            // validate() keeps a score at two levels or more
            let top = (criteria.len() - 1) as f64;
            if !(0.0..=top).contains(&score) {
                return Err("the score is outside the scale");
            }
            Ok(Answer::Score {
                score: (score / top) as f32,
                probabilities,
                confidence: unit(confidence, "the confidence is outside 0–1")?,
            })
        }
        _ => Err("the answer is of a different type than the question"),
    }
}

fn unit(value: f64, reason: &'static str) -> Result<f32, &'static str> {
    if (0.0..=1.0).contains(&value) {
        Ok(value as f32)
    } else {
        Err(reason)
    }
}

/// The probabilities in the question's order, when they are keyed exactly by `keys`, each is
/// within 0–1, and together they sum to 1.
fn distribution<'a>(
    probabilities: &IndexMap<String, f64>,
    keys: impl ExactSizeIterator<Item = &'a str>,
    mismatch: &'static str,
) -> Result<IndexMap<String, f32>, &'static str> {
    if probabilities.len() != keys.len() {
        return Err(mismatch);
    }
    let mut ordered = IndexMap::with_capacity(probabilities.len());
    let mut sum = 0.0;
    for key in keys {
        let p = *probabilities.get(key).ok_or(mismatch)?;
        sum += p;
        ordered.insert(key.to_owned(), unit(p, "a probability is outside 0–1")?);
    }
    if (sum - 1.0).abs() > SUM_TOLERANCE {
        return Err("the probabilities don't sum to 1");
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn class() -> Question {
        Question::Choice {
            instructions: json!("The kind of question it is."),
            criteria: IndexMap::from([
                ("progress".to_owned(), Value::Null),
                ("tested".to_owned(), Value::Null),
            ]),
        }
    }

    fn choose(choice: &str, progress: f64, tested: f64) -> Result<Answer, &'static str> {
        check(
            &class(),
            json!({"type": "choice", "choice": choice,
                   "probabilities": {"progress": progress, "tested": tested}, "confidence": 0.5}),
        )
    }

    #[test]
    fn a_pick_only_a_little_less_probable_than_another_option_is_not_the_most_probable() {
        assert_eq!(
            choose("progress", 0.495, 0.505),
            Err("the choice is not the most probable option"),
            "0.01 below: the sum's rounding allowance is not a tie"
        );
        assert_eq!(
            choose("progress", 0.49, 0.51),
            Err("the choice is not the most probable option")
        );
    }

    #[test]
    fn a_tie_lets_either_option_be_the_pick() {
        assert!(choose("progress", 0.5, 0.5).is_ok());
        assert!(choose("tested", 0.5, 0.5).is_ok());
        // f32 can't tell these apart: still a tie
        assert!(choose("progress", 0.500_000_01, 0.499_999_99).is_ok());
        assert!(choose("tested", 0.500_000_01, 0.499_999_99).is_ok());
    }
}
