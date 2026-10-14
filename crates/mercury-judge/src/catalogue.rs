//! The check catalogue: `checks.yaml`, checked against the table of checks Mercury knows, and the
//! verdict each answer turns into.

use std::path::Path;

use mercury_schema::JudgeMode;
use mercury_schema::Verdict;
use mercury_schema::config::{CheckConfig, CheckStage, CheckType, ChecksConfig, Polarity, Thresholds};
use mercury_schema::facts::Ratio;

use crate::Answer;

/// Every check Mercury knows: its id, stage, type and polarity. `checks.yaml` must list each of
/// them, with exactly these.
pub const KNOWN: [(&str, CheckStage, CheckType, Option<Polarity>); 18] = {
    use CheckStage::{Data, Input, Output, Tool};
    use CheckType::{Choice, Noul, Rule};
    use Polarity::{Legit, Risk};
    [
        ("project", Input, Choice, None),
        ("class", Input, Choice, None),
        ("asks_for_code", Input, Noul, Some(Risk)),
        ("asks_to_evaluate_people", Input, Noul, Some(Risk)),
        ("asks_for_client_commitment", Input, Noul, Some(Risk)),
        ("contains_agent_instruction", Input, Noul, Some(Risk)),
        ("asks_for_confidential", Input, Noul, Some(Risk)),
        ("scope", Tool, Rule, None),
        ("deny_class", Tool, Rule, None),
        ("args_match_question", Tool, Noul, Some(Legit)),
        ("args_free_of_instructions", Tool, Noul, Some(Legit)),
        ("field_contains_agent_instruction", Data, Noul, Some(Risk)),
        ("sentence_contains_code_or_identifier", Output, Noul, Some(Risk)),
        ("sentence_contains_path_or_secret", Output, Noul, Some(Risk)),
        ("sentence_mentions_client", Output, Noul, Some(Risk)),
        ("sentence_evaluates_people", Output, Noul, Some(Risk)),
        ("sentence_commits_to_client", Output, Noul, Some(Risk)),
        ("grounded", Output, Noul, Some(Legit)),
    ]
};

/// How far every threshold moves toward refusing when the judge isn't Jev.
pub const TIGHTEN_BY: f64 = 0.2;

/// A tightened threshold stays within these: a probability of 0 is a no and 1 a yes, so a
/// threshold at either end would turn every answer into a review or a refusal.
pub const TIGHTENED_FLOOR: f64 = 0.05;
pub const TIGHTENED_CEILING: f64 = 0.95;

/// Why `checks.yaml` was refused. Each message names the check id, never its instructions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogueError {
    #[error("the check catalogue can't be read: {0}")]
    Read(String),
    #[error("the check catalogue is invalid at line {line}, column {column}")]
    Parse { line: usize, column: usize },
    #[error("the check catalogue is invalid")]
    Invalid,
    #[error("`{0}` isn't a check Mercury knows")]
    UnknownCheck(String),
    #[error("`{0}` has the wrong stage")]
    WrongStage(String),
    #[error("`{0}` has the wrong type")]
    WrongType(String),
    #[error("`{0}` has the wrong polarity, or none")]
    WrongPolarity(String),
    #[error("`{0}` needs its instructions")]
    NoInstructions(String),
    #[error("`{0}` is missing a threshold its type needs")]
    MissingThreshold(String),
    #[error("`{0}` has thresholds in the wrong order")]
    ThresholdOrder(String),
    #[error("`{0}` asks per item but its `per_item` has no `{{i}}`")]
    PerItem(String),
    #[error("the catalogue doesn't list `{0}`")]
    MissingCheck(String),
}

/// A loaded, checked catalogue.
#[derive(Debug, Clone)]
pub struct Catalogue {
    checks: Vec<CheckConfig>,
}

impl Catalogue {
    pub fn load(path: &Path) -> Result<Self, CatalogueError> {
        let yaml =
            std::fs::read_to_string(path).map_err(|err| CatalogueError::Read(err.kind().to_string()))?;
        Self::from_yaml(&yaml)
    }

    pub fn from_yaml(yaml: &str) -> Result<Self, CatalogueError> {
        let config: ChecksConfig = serde_norway::from_str(yaml).map_err(|err| match err.location() {
            Some(at) => CatalogueError::Parse {
                line: at.line(),
                column: at.column(),
            },
            None => CatalogueError::Invalid,
        })?;
        Self::new(config)
    }

    pub fn new(config: ChecksConfig) -> Result<Self, CatalogueError> {
        for check in &config.0 {
            validate(check)?;
        }
        for (id, ..) in KNOWN {
            if !config.0.iter().any(|c| c.id.as_str() == id) {
                return Err(CatalogueError::MissingCheck(id.to_owned()));
            }
        }
        Ok(Self { checks: config.0 })
    }

    pub fn get(&self, id: &str) -> Option<&CheckConfig> {
        self.checks.iter().find(|c| c.id.as_str() == id)
    }

    /// The checks of a stage that a judge answers, in catalogue order: rule checks are code's.
    pub fn asked(&self, stage: CheckStage) -> impl Iterator<Item = &CheckConfig> {
        self.checks
            .iter()
            .filter(move |c| c.stage == stage && c.check_type != CheckType::Rule)
    }
}

fn validate(check: &CheckConfig) -> Result<(), CatalogueError> {
    let id = check.id.as_str().to_owned();
    let &(_, stage, check_type, polarity) = KNOWN
        .iter()
        .find(|(known, ..)| *known == id)
        .ok_or_else(|| CatalogueError::UnknownCheck(id.clone()))?;
    if check.stage != stage {
        return Err(CatalogueError::WrongStage(id));
    }
    if check.check_type != check_type {
        return Err(CatalogueError::WrongType(id));
    }
    if check.polarity != polarity {
        return Err(CatalogueError::WrongPolarity(id));
    }
    let t = &check.thresholds;
    let get = |r: Option<Ratio>| r.map(Ratio::get);
    match (check_type, polarity) {
        (CheckType::Rule, _) => {}
        (CheckType::Choice, _) => {
            if t.clarify_below.is_none() {
                return Err(CatalogueError::MissingThreshold(id));
            }
        }
        (CheckType::Noul, Some(Polarity::Risk)) => {
            let block = get(t.block_at).ok_or_else(|| CatalogueError::MissingThreshold(id.clone()))?;
            if get(t.review_at).is_some_and(|review| review >= block) {
                return Err(CatalogueError::ThresholdOrder(id));
            }
        }
        (CheckType::Noul, _) => {
            let (Some(pass), Some(block)) = (get(t.pass_at), get(t.block_at)) else {
                return Err(CatalogueError::MissingThreshold(id));
            };
            if block > pass {
                return Err(CatalogueError::ThresholdOrder(id));
            }
        }
    }
    if check_type == CheckType::Noul && check.instructions.as_deref().is_none_or(|i| i.trim().is_empty()) {
        return Err(CatalogueError::NoInstructions(id));
    }
    if matches!(stage, CheckStage::Data | CheckStage::Output)
        && check_type == CheckType::Noul
        && !check.per_item.as_deref().is_some_and(|p| p.contains("{i}"))
    {
        return Err(CatalogueError::PerItem(id));
    }
    Ok(())
}

/// Why an answer gave no verdict. Never a pass.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerdictError {
    #[error("`{0}` wasn't answered")]
    Unanswered(String),
    #[error("`{0}` was answered with another type of answer")]
    WrongAnswer(String),
    #[error("`{0}` is a rule check; code decides it")]
    RuleCheck(String),
}

/// The thresholds a check uses in a mode: as configured for Jev, 0.2 toward refusing otherwise.
pub fn thresholds(check: &CheckConfig, mode: JudgeMode) -> Thresholds {
    let t = check.thresholds;
    if mode == JudgeMode::Jev {
        return t;
    }
    let shift = |r: Option<Ratio>, by: f64| {
        r.and_then(|r| {
            let moved = r.get() + by;
            // never past the end it moves toward, and never back past where it started
            let held = if by < 0.0 {
                moved.max(TIGHTENED_FLOOR.min(r.get()))
            } else {
                moved.min(TIGHTENED_CEILING.max(r.get()))
            };
            Ratio::new(held)
        })
    };
    match check.polarity {
        Some(Polarity::Risk) => Thresholds {
            block_at: shift(t.block_at, -TIGHTEN_BY),
            review_at: shift(t.review_at, -TIGHTEN_BY),
            ..t
        },
        // legit checks and choices both refuse more by asking for more
        _ => Thresholds {
            pass_at: shift(t.pass_at, TIGHTEN_BY),
            block_at: shift(t.block_at, TIGHTEN_BY),
            clarify_below: shift(t.clarify_below, TIGHTEN_BY),
            ..t
        },
    }
}

/// How far below a threshold a probability may sit and still reach it: the rounding of an `f32`
/// answer, not a difference the judge meant.
const ROUNDING: f64 = 1e-6;

/// What one answer means for one check. A missing answer is an error, never a pass.
pub fn verdict(
    check: &CheckConfig,
    answer: Option<&Answer>,
    mode: JudgeMode,
) -> Result<Verdict, VerdictError> {
    let id = check.id.as_str().to_owned();
    let answer = answer.ok_or_else(|| VerdictError::Unanswered(id.clone()))?;
    let t = thresholds(check, mode);
    let at = |r: Option<Ratio>| r.map(Ratio::get);
    match (check.check_type, answer) {
        (CheckType::Rule, _) => Err(VerdictError::RuleCheck(id)),
        (
            CheckType::Choice,
            Answer::Choice {
                choice,
                probabilities,
                ..
            },
        ) => {
            let p = f64::from(probabilities.get(choice).copied().unwrap_or(0.0));
            // a missing threshold would have failed the load; read as the strictest
            Ok(if p >= at(t.clarify_below).unwrap_or(1.0) - ROUNDING {
                Verdict::Pass
            } else {
                Verdict::Review
            })
        }
        (CheckType::Noul, Answer::Noul(p)) => {
            let p = f64::from(*p);
            Ok(match check.polarity {
                Some(Polarity::Risk) => {
                    if p >= at(t.block_at).unwrap_or(0.0) - ROUNDING {
                        Verdict::Block
                    } else if at(t.review_at).is_some_and(|review| p >= review - ROUNDING) {
                        Verdict::Review
                    } else {
                        Verdict::Pass
                    }
                }
                _ => {
                    if p >= at(t.pass_at).unwrap_or(f64::INFINITY) - ROUNDING {
                        Verdict::Pass
                    } else if p <= at(t.block_at).unwrap_or(f64::INFINITY) + ROUNDING {
                        Verdict::Block
                    } else {
                        Verdict::Review
                    }
                }
            })
        }
        _ => Err(VerdictError::WrongAnswer(id)),
    }
}
