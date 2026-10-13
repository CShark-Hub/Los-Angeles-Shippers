//! `checks.yaml`: the judge's check catalogue, one entry per check.
//!
//! Which thresholds a check needs depends on its type and polarity; the catalogue's loader checks
//! that. This file only fixes the keys and their ranges.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::facts::Ratio;
use crate::ids::CheckId;

/// The catalogue: a list of checks, each id once.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ChecksConfig(pub Vec<CheckConfig>);

impl<'de> Deserialize<'de> for ChecksConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let checks = Vec::<CheckConfig>::deserialize(deserializer)?;
        let mut seen = HashSet::new();
        for check in &checks {
            if !seen.insert(&check.id) {
                return Err(serde::de::Error::custom(format!(
                    "the check `{}` is listed twice",
                    check.id
                )));
            }
        }
        Ok(Self(checks))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckConfig {
    pub id: CheckId,
    pub stage: CheckStage,
    #[serde(rename = "type")]
    pub check_type: CheckType,
    /// For `noul` checks: `risk` (high is bad) or `legit` (high is good).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity: Option<Polarity>,
    /// For a check asked once per item, the item's place in the state, e.g.
    /// `answer.sentences[{i}]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_item: Option<String>,
    /// The proposition the judge rates, about the state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub thresholds: Thresholds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStage {
    /// The question, before anything runs.
    Input,
    /// The planned tool calls.
    Tool,
    /// Each text value a probe returned.
    Data,
    /// Each sentence of the composed answer.
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    /// Pick one of several options.
    Choice,
    /// A probability that a proposition holds.
    Noul,
    /// Decided by code, without a model.
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    /// High is bad.
    Risk,
    /// High is good.
    Legit,
}

/// Where a probability turns into a verdict. A `risk` check blocks at or above `block_at` and
/// goes to review at or above `review_at`; a `legit` check passes at or above `pass_at` and
/// blocks at or below `block_at`; a `choice` asks the person when below `clarify_below`. The
/// chat fallback tightens each by 0.2.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_at: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_at: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_at: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarify_below: Option<Ratio>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKS: &str = r#"
- id: class
  stage: input
  type: choice
  thresholds: { clarify_below: 0.6 }
- id: asks_for_code
  stage: input
  type: noul
  polarity: risk
  instructions: >-
    `question.text` asks to see source code, code snippets, names of functions,
    classes or files, or file paths.
  thresholds: { block_at: 0.5 }
- id: sentence_contains_code_or_identifier
  stage: output
  type: noul
  polarity: risk
  per_item: "answer.sentences[{i}]"
  instructions: >-
    `answer.sentences[{i}]` contains source code or a name of a function, class,
    variable, file or folder.
  thresholds: { block_at: 0.8, review_at: 0.2 }
- id: scope
  stage: tool
  type: rule
"#;

    fn parse(yaml: &str) -> Result<ChecksConfig, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    #[test]
    fn the_documented_checks_parse() {
        let c = parse(CHECKS).unwrap();
        assert_eq!(c.0.len(), 4);
        let code = &c.0[1];
        assert_eq!(code.check_type, CheckType::Noul);
        assert_eq!(code.polarity, Some(Polarity::Risk));
        assert_eq!(code.thresholds.block_at.map(Ratio::get), Some(0.5));
        assert_eq!(c.0[2].per_item.as_deref(), Some("answer.sentences[{i}]"));
        assert_eq!(c.0[3].thresholds, Thresholds::default());
    }

    #[test]
    fn unknown_keys_and_values_are_rejected() {
        assert!(
            parse(&CHECKS.replace(
                "polarity: risk\n  instructions",
                "polarity: risky\n  instructions"
            ))
            .is_err()
        );
        assert!(parse(&CHECKS.replace("{ block_at: 0.5 }", "{ block_at: 0.5, warn_at: 0.3 }")).is_err());
        assert!(parse(&CHECKS.replace("stage: tool", "stage: planner")).is_err());
        assert!(parse(&CHECKS.replace("type: rule", "type: rule\n  weight: 2")).is_err());
    }

    #[test]
    fn thresholds_are_probabilities() {
        assert!(parse(&CHECKS.replace("{ block_at: 0.5 }", "{ block_at: 1.5 }")).is_err());
    }

    #[test]
    fn a_check_is_listed_once() {
        assert!(parse(&format!("{CHECKS}- id: scope\n  stage: tool\n  type: rule\n")).is_err());
    }
}
