//! The judge at each stage. The chain (Jev, then the chat judge) answers the stage's one request;
//! when no judge of it answers, the stage's own rules do, and the mode says so. A judge error is
//! never a pass. At the output stage the rules that back a check (the citation, the code, path and
//! secret patterns, the client names) run in every mode, and the stricter verdict stands.

use std::sync::Arc;

use mercury_judge::stages::{self, RuleContext, StageBatch, StageError, StageVerdicts};
use mercury_judge::{Answer, Catalogue, Judge};
use mercury_schema::config::Polarity;
use mercury_schema::events::{CheckResult, CheckSource};
use mercury_schema::facts::Ratio;
use mercury_schema::{CheckId, JudgeMode, Verdict};

/// The output checks a rule backs in every mode, not only when the judges are down.
pub const RULE_BACKED: [&str; 4] = [
    "grounded",
    "sentence_contains_code_or_identifier",
    "sentence_contains_path_or_secret",
    "sentence_mentions_client",
];

#[derive(Clone)]
pub struct Judging {
    chain: Arc<dyn Judge>,
    catalogue: Arc<Catalogue>,
}

impl Judging {
    pub fn new(chain: Arc<dyn Judge>, catalogue: Arc<Catalogue>) -> Self {
        Self { chain, catalogue }
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// One stage's verdicts: the chain's, or the stage's rules' when no judge of the chain answers.
    pub async fn decide(&self, stage: &StageBatch, rules: &RuleContext) -> Result<StageVerdicts, StageError> {
        match stages::decide(self.chain.as_ref(), &self.catalogue, stage).await {
            Ok(verdicts) => Ok(verdicts),
            Err(StageError::Judge(err)) => {
                tracing::warn!(stage = ?stage.stage, error = %err, "no judge answered; the rules decide the stage");
                self.rules(stage, rules).await
            }
            Err(StageError::Verdict(err)) => {
                tracing::warn!(stage = ?stage.stage, error = %err, "a judge's answer gives no verdict; the rules decide the stage");
                self.rules(stage, rules).await
            }
            Err(err) => Err(err),
        }
    }

    async fn rules(&self, stage: &StageBatch, rules: &RuleContext) -> Result<StageVerdicts, StageError> {
        stages::decide(&stages::rules_for(stage, rules), &self.catalogue, stage).await
    }

    /// The output stage: the judge's verdicts, each raised to its rule's for a check a rule backs.
    pub async fn decide_output(
        &self,
        stage: &StageBatch,
        rules: &RuleContext,
    ) -> Result<StageVerdicts, StageError> {
        let mut decided = self.decide(stage, rules).await?;
        if decided.outcome.mode == JudgeMode::Rules {
            return Ok(decided);
        }
        let ruled = self.rules(stage, rules).await?;
        for (key, verdict) in decided.verdicts.iter_mut() {
            let backed = stage
                .keys
                .get(key)
                .is_some_and(|(id, _)| RULE_BACKED.contains(&id.as_str()));
            if let (true, Some(rule)) = (backed, ruled.verdicts.get(key)) {
                *verdict = (*verdict).max(*rule);
            }
        }
        Ok(decided)
    }
}

/// Who decided, as a check's `source` reads.
pub fn source(mode: JudgeMode) -> CheckSource {
    match mode {
        JudgeMode::Jev => CheckSource::Jev,
        JudgeMode::Chat => CheckSource::Chat,
        JudgeMode::Rules => CheckSource::Rule,
    }
}

/// A probability as an event shows it: four places, without the noise of a 32-bit float. The
/// verdicts read the judge's own value.
fn rounded(p: f64) -> f64 {
    (p * 10_000.0).round() / 10_000.0
}

/// A `noul` answer's probability; `None` for a rule, which has none to show.
fn probability(answer: Option<&Answer>, mode: JudgeMode) -> Option<Ratio> {
    if mode == JudgeMode::Rules {
        return None;
    }
    match answer {
        Some(Answer::Noul(p)) => Ratio::new(rounded(f64::from(*p))),
        _ => None,
    }
}

/// The lowest probability that a stage's checks pass, over the items given (`f{i}`, `s{i}`) and the
/// checks a judge answered: a `legit` check's `p` is the chance the thing is fine, a `risk` check's
/// is the chance it isn't, so the chance it is fine is `1 - p`. Rounded as an event shows a
/// probability. `None` when only the rules decided (they have none to give) or no check was asked of
/// those items. One definition, for `tool_result` and `judged` alike.
pub fn p_min(
    stage: &StageBatch,
    decided: &StageVerdicts,
    items: &[usize],
    catalogue: &Catalogue,
) -> Option<Ratio> {
    if decided.outcome.mode == JudgeMode::Rules {
        return None;
    }
    stage
        .keys
        .iter()
        .filter(|(_, (_, at))| at.is_some_and(|item| items.contains(&item)))
        .filter_map(|(key, (id, _))| {
            let Some(Answer::Noul(p)) = decided.outcome.answers.get(key) else {
                return None;
            };
            let p = f64::from(*p);
            match catalogue.get(id)?.polarity? {
                Polarity::Legit => Some(p),
                Polarity::Risk => Some(1.0 - p),
            }
        })
        .min_by(f64::total_cmp)
        .and_then(|p| Ratio::new(rounded(p)))
}

/// The results of the stage's checks with these ids, in the stage's order, for an event.
pub fn results(stage: &StageBatch, decided: &StageVerdicts, ids: &[&str]) -> Vec<CheckResult> {
    let mode = decided.outcome.mode;
    stage
        .keys
        .iter()
        .filter(|(_, (id, _))| ids.contains(&id.as_str()))
        .filter_map(|(key, (id, _))| {
            Some(CheckResult {
                id: CheckId::parse(id).ok()?,
                source: source(mode),
                p: probability(decided.outcome.answers.get(key), mode),
                verdict: decided.verdicts.get(key).copied().unwrap_or(Verdict::Block),
            })
        })
        .collect()
}

/// A rule check of the tool stage, which code decides.
pub fn rule(id: &str, verdict: Verdict) -> Option<CheckResult> {
    Some(CheckResult {
        id: CheckId::parse(id).ok()?,
        source: CheckSource::Rule,
        p: None,
        verdict,
    })
}

/// A choice's pick and its probability.
pub fn pick(decided: &StageVerdicts, key: &str) -> Option<(String, f64)> {
    match decided.outcome.answers.get(key) {
        Some(Answer::Choice {
            choice,
            probabilities,
            ..
        }) => Some((
            choice.clone(),
            rounded(f64::from(probabilities.get(choice).copied().unwrap_or(0.0))),
        )),
        _ => None,
    }
}

/// The checks that put one item (`s{i}.…`) in their review band, each with the probability the
/// approver is shown: the judge's, or the rule's when the rules decided.
pub fn reviews(stage: &StageBatch, decided: &StageVerdicts, item: usize) -> Vec<(CheckId, Ratio)> {
    stage
        .keys
        .iter()
        .filter(|(key, (_, at))| *at == Some(item) && decided.verdicts.get(*key) == Some(&Verdict::Review))
        .filter_map(|(key, (id, _))| {
            let p = match decided.outcome.answers.get(key)? {
                Answer::Noul(p) => f64::from(*p),
                Answer::Choice {
                    choice,
                    probabilities,
                    ..
                } => f64::from(probabilities.get(choice).copied().unwrap_or(0.0)),
                _ => return None,
            };
            Some((CheckId::parse(id).ok()?, Ratio::new(rounded(p))?))
        })
        .collect()
}

/// The strictest verdict of one item's checks (`s{i}.…`, `f{i}.…`), or of every check without an
/// item; a key the stage didn't answer counts as a block.
pub fn strictest(stage: &StageBatch, decided: &StageVerdicts, item: Option<usize>) -> Verdict {
    stage
        .keys
        .iter()
        .filter(|(_, (_, at))| *at == item)
        .map(|(key, _)| decided.verdicts.get(key).copied().unwrap_or(Verdict::Block))
        .max()
        .unwrap_or(Verdict::Pass)
}
