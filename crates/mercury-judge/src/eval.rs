//! The evaluation harness behind `just judge-eval`: the labelled sets through a judge, one stage
//! request per item, and a report of how each check did.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use mercury_schema::Verdict;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::catalogue::Catalogue;
use crate::stages::{self, InputContext, PlannedCall, RuleContext, StageBatch, StageError, ToolModule};
use crate::{Judge, JudgeError};

/// The input stage's risk checks, in catalogue order.
pub const INPUT_RISKS: [&str; 5] = [
    "asks_for_code",
    "asks_to_evaluate_people",
    "asks_for_client_commitment",
    "contains_agent_instruction",
    "asks_for_confidential",
];

/// The output stage's checks, in catalogue order.
pub const OUTPUT_CHECKS: [&str; 6] = [
    "sentence_contains_code_or_identifier",
    "sentence_contains_path_or_secret",
    "sentence_mentions_client",
    "sentence_evaluates_people",
    "sentence_commits_to_client",
    "grounded",
];

/// The output checks a repository title can fail.
pub const TITLE_CHECKS: [&str; 2] = [
    "sentence_contains_code_or_identifier",
    "sentence_contains_path_or_secret",
];

// ---------------------------------------------------------------------------------------- the sets

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionItem {
    pub id: String,
    pub lang: String,
    pub role: String,
    pub project: String,
    pub text: String,
    pub expect: QuestionExpect,
    /// Written after the thresholds were set and kept apart from the part they were tuned on.
    #[serde(default)]
    pub held_out: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionExpect {
    pub class: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    pub refuse: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub also: Vec<String>,
}

impl QuestionExpect {
    /// The input checks that must fire: the reason and `also`, when the reason is a check. A
    /// policy refusal (`forbidden_class`, `forbidden_project`) is the policy's, not a check's.
    pub fn fires(&self) -> Vec<&str> {
        if !self.refuse {
            return Vec::new();
        }
        self.reason
            .iter()
            .chain(self.also.iter())
            .map(String::as_str)
            .filter(|r| INPUT_RISKS.contains(r))
            .collect()
    }

    /// Refused by a check, rather than by the policy.
    pub fn refused_by_a_check(&self) -> bool {
        !self.fires().is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentenceItem {
    pub id: String,
    pub lang: String,
    pub text: String,
    pub facts: Vec<String>,
    pub expect: SentenceExpect,
    #[serde(default)]
    pub held_out: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentenceExpect {
    pub fires: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleItem {
    pub id: String,
    pub text: String,
    pub expect: TitleExpect,
    #[serde(default)]
    pub held_out: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleExpect {
    #[serde(rename = "unsafe")]
    pub is_unsafe: bool,
}

/// The tool stage's checks, in catalogue order.
pub const TOOL_CHECKS: [&str; 2] = ["args_match_question", "args_free_of_instructions"];

/// One planned call of a plan: what the planner filled for a question.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanCall {
    pub tool: String,
    pub args: serde_json::Value,
}

/// A question and the plan made for it: the tool stage's request, with the tool checks that must
/// not pass it (`expect.fires`). A plan that fills what the question asks passes both; a wrong
/// module, a wider scope or an instruction in an argument must not.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub id: String,
    pub lang: String,
    pub project: String,
    pub text: String,
    pub plan: Vec<PlanCall>,
    pub expect: SentenceExpect,
    #[serde(default)]
    pub held_out: bool,
}

/// The labelled sets, and the modules of the projects the plans are about (`projects.json`: project →
/// module → labels, as the manifest gives them).
#[derive(Debug, Clone)]
pub struct Sets {
    pub questions: Vec<QuestionItem>,
    pub sentences: Vec<SentenceItem>,
    pub titles: Vec<TitleItem>,
    pub plans: Vec<PlanItem>,
    pub modules: BTreeMap<String, Vec<ToolModule>>,
}

impl Sets {
    /// The items written after the thresholds were set (`held_out`), or the ones they were tuned on.
    pub fn part(&self, held_out: bool) -> Self {
        fn keep<T: Clone>(items: &[T], held_out: bool, is: impl Fn(&T) -> bool) -> Vec<T> {
            items
                .iter()
                .filter(|item| is(item) == held_out)
                .cloned()
                .collect()
        }
        Self {
            questions: keep(&self.questions, held_out, |q| q.held_out),
            sentences: keep(&self.sentences, held_out, |s| s.held_out),
            titles: keep(&self.titles, held_out, |t| t.held_out),
            plans: keep(&self.plans, held_out, |p| p.held_out),
            modules: self.modules.clone(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("projects.json");
        let text =
            std::fs::read_to_string(&path).map_err(|err| format!("cannot read projects.json: {err}"))?;
        let projects: BTreeMap<String, BTreeMap<String, Vec<String>>> =
            serde_json::from_str(&text).map_err(|_| "projects.json is invalid".to_owned())?;
        Ok(Self {
            questions: jsonl(&dir.join("questions.jsonl"))?,
            sentences: jsonl(&dir.join("sentences.jsonl"))?,
            titles: jsonl(&dir.join("titles.jsonl"))?,
            plans: jsonl(&dir.join("plans.jsonl"))?,
            modules: projects
                .into_iter()
                .map(|(project, modules)| {
                    let modules = modules
                        .into_iter()
                        .map(|(name, labels)| ToolModule { name, labels })
                        .collect();
                    (project, modules)
                })
                .collect(),
        })
    }
}

fn jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("a set");
    let text = std::fs::read_to_string(path).map_err(|err| format!("cannot read {name}: {err}"))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| serde_json::from_str(line).map_err(|_| format!("{name} line {} is invalid", i + 1)))
        .collect()
}

// ---------------------------------------------------------------------------------------- results

/// One check's counts: fired when it should (TP), fired when it shouldn't (FP), and so on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub tp: u32,
    pub fp: u32,
    pub fn_: u32,
    pub tn: u32,
}

impl Counts {
    pub fn add(&mut self, expected: bool, fired: bool) {
        match (expected, fired) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
    }

    pub fn n(&self) -> u32 {
        self.tp + self.fp + self.fn_ + self.tn
    }

    pub fn accuracy(&self) -> Option<f64> {
        ratio(self.tp + self.tn, self.n())
    }

    pub fn precision(&self) -> Option<f64> {
        ratio(self.tp, self.tp + self.fp)
    }

    pub fn recall(&self) -> Option<f64> {
        ratio(self.tp, self.tp + self.fn_)
    }
}

fn ratio(a: u32, b: u32) -> Option<f64> {
    (b > 0).then(|| f64::from(a) / f64::from(b))
}

/// What one backend did on the sets.
#[derive(Debug, Clone, Default)]
pub struct BackendReport {
    pub name: String,
    /// `None` when it ran; otherwise why it didn't.
    pub skipped: Option<String>,
    pub checks: BTreeMap<String, Counts>,
    /// Expected class → picked class (`clarify` when the pick was below the threshold).
    pub classes: BTreeMap<String, BTreeMap<String, u32>>,
    /// Per language: forbidden by a check, refused, allowed, wrongly refused.
    pub refusals: BTreeMap<String, [u32; 4]>,
    pub latencies_ms: Vec<u32>,
    pub cost_usd: f64,
    pub unknown_cost: u32,
    pub requests: u32,
    pub errors: u32,
    /// Items that failed, by id, with the error's kind: never counted as a pass.
    pub failed: Vec<(String, String)>,
    /// What the judge answered, for `--dump`: the item's id, the question's key and the answer (a
    /// probability, or the option picked and its probability).
    pub readings: Vec<(String, String, String)>,
}

impl BackendReport {
    fn record(&mut self, id: &str, result: &Result<stages::StageVerdicts, StageError>) {
        self.requests += 1;
        match result {
            Ok(decided) => {
                self.latencies_ms.push(decided.outcome.latency_ms);
                match decided.outcome.cost_usd {
                    Some(cost) => self.cost_usd += cost,
                    None => self.unknown_cost += 1,
                }
            }
            Err(err) => {
                self.errors += 1;
                self.failed.push((id.to_owned(), short(err)));
            }
        }
    }

    /// Keeps what a judge answered to an item, to be dumped.
    fn read(&mut self, id: &str, decided: &stages::StageVerdicts) {
        for (key, answer) in &decided.outcome.answers {
            let shown = match answer {
                crate::Answer::Noul(p) => format!("{p:.3}"),
                crate::Answer::Choice {
                    choice,
                    probabilities,
                    ..
                } => {
                    format!(
                        "{choice} {:.3}",
                        probabilities.get(choice).copied().unwrap_or_default()
                    )
                }
                crate::Answer::Score { score, .. } => format!("{score:.3}"),
            };
            self.readings.push((id.to_owned(), key.clone(), shown));
        }
    }

    pub fn percentile(&self, p: f64) -> Option<u32> {
        if self.latencies_ms.is_empty() {
            return None;
        }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_unstable();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into a short list"
        )]
        let i = ((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
        Some(sorted[i])
    }
}

fn short(err: &StageError) -> String {
    match err {
        StageError::Judge(JudgeError::NoKey) => "no key".into(),
        StageError::Judge(JudgeError::Timeout(_)) => "timeout".into(),
        StageError::Judge(JudgeError::Status(code)) => format!("HTTP {code}"),
        StageError::Judge(other) => other.to_string(),
        StageError::Verdict(err) => err.to_string(),
        StageError::UnknownCheck(id) => format!("unknown check {id}"),
    }
}

/// What the evaluation runs with besides the judge.
pub struct EvalContext<'a> {
    pub catalogue: &'a Catalogue,
    /// The redaction list, for `sentence_mentions_client`'s rule.
    pub clients: Vec<Regex>,
    /// Only this check, when set.
    pub only: Option<&'a str>,
    /// Whether the tool stage's state carries the project's modules and their labels, as the core
    /// sends it. `false` leaves them out, for a report to measure what they add.
    pub modules: bool,
}

/// Runs every set through one judge. `rules` makes the rules judge for each stage's batch; it is
/// used only when the judge is the rules judge (`None` for the others).
pub async fn run(name: &str, judge: Option<&dyn Judge>, sets: &Sets, ctx: &EvalContext<'_>) -> BackendReport {
    let mut report = BackendReport {
        name: name.to_owned(),
        ..BackendReport::default()
    };
    let wanted = |check: &str| ctx.only.is_none_or(|only| only == check);

    for item in &sets.questions {
        let projects = vec![item.project.clone()];
        let stage = stages::input_batch(
            ctx.catalogue,
            &InputContext {
                text: &item.text,
                role: &item.role,
                project: Some(&item.project),
                projects: &projects,
            },
        );
        let rules_ctx = RuleContext {
            projects,
            ..RuleContext::default()
        };
        let result = ask(judge, ctx.catalogue, &stage, &rules_ctx).await;
        report.record(&item.id, &result);
        let Ok(decided) = result else { continue };
        report.read(&item.id, &decided);
        let expected = item.expect.fires();
        let mut refused = false;
        for check in INPUT_RISKS {
            let fired = decided.verdicts.get(check).is_some_and(|v| *v != Verdict::Pass);
            refused |= fired;
            if wanted(check) {
                report
                    .checks
                    .entry(check.to_owned())
                    .or_default()
                    .add(expected.contains(&check), fired);
            }
        }
        if wanted("class")
            && let Some(class) = &item.expect.class
        {
            let picked = match (decided.verdicts.get("class"), &decided.class) {
                (Some(Verdict::Pass), Some(pick)) => pick.clone(),
                _ => "clarify".to_owned(),
            };
            refused |= picked == "forbidden";
            *report
                .classes
                .entry(class.clone())
                .or_default()
                .entry(picked.clone())
                .or_default() += 1;
            report
                .checks
                .entry("class".to_owned())
                .or_default()
                .add(true, &picked == class);
        }
        let row = report.refusals.entry(item.lang.clone()).or_default();
        if item.expect.refused_by_a_check() {
            row[0] += 1;
            row[1] += u32::from(refused);
        } else if !item.expect.refuse {
            row[2] += 1;
            row[3] += u32::from(refused);
        }
    }

    for item in &sets.sentences {
        let stage = stages::output_batch(
            ctx.catalogue,
            std::slice::from_ref(&item.text),
            &json!(
                item.facts
                    .iter()
                    .map(|id| json!({ "id": id }))
                    .collect::<Vec<_>>()
            ),
        );
        let rules_ctx = RuleContext {
            client_patterns: ctx.clients.clone(),
            fact_ids: item.facts.clone(),
            projects: Vec::new(),
        };
        let result = ask(judge, ctx.catalogue, &stage, &rules_ctx).await;
        report.record(&item.id, &result);
        let Ok(decided) = result else { continue };
        report.read(&item.id, &decided);
        for check in OUTPUT_CHECKS.iter().filter(|c| wanted(c)) {
            let fired = decided
                .verdicts
                .get(&format!("s0.{check}"))
                .is_some_and(|v| *v != Verdict::Pass);
            report
                .checks
                .entry((*check).to_owned())
                .or_default()
                .add(item.expect.fires.iter().any(|f| f == check), fired);
        }
    }

    if TITLE_CHECKS.iter().any(|c| wanted(c)) {
        for item in &sets.titles {
            let stage = stages::output_batch(ctx.catalogue, std::slice::from_ref(&item.text), &json!([]));
            let rules_ctx = RuleContext {
                client_patterns: ctx.clients.clone(),
                ..RuleContext::default()
            };
            let result = ask(judge, ctx.catalogue, &stage, &rules_ctx).await;
            report.record(&item.id, &result);
            let Ok(decided) = result else { continue };
            report.read(&item.id, &decided);
            let fired = TITLE_CHECKS.iter().any(|check| {
                decided
                    .verdicts
                    .get(&format!("s0.{check}"))
                    .is_some_and(|v| *v != Verdict::Pass)
            });
            report
                .checks
                .entry("title_unsafe".to_owned())
                .or_default()
                .add(item.expect.is_unsafe, fired);
        }
    }
    if TOOL_CHECKS.iter().any(|c| wanted(c)) {
        for item in &sets.plans {
            let calls: Vec<PlannedCall> = item
                .plan
                .iter()
                .map(|call| PlannedCall {
                    tool: call.tool.clone(),
                    args: call.args.clone(),
                })
                .collect();
            let modules = if ctx.modules {
                sets.modules.get(&item.project).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            let stage = stages::tool_batch(ctx.catalogue, &item.text, &calls, &modules);
            let result = ask(judge, ctx.catalogue, &stage, &RuleContext::default()).await;
            report.record(&item.id, &result);
            let Ok(decided) = result else { continue };
            report.read(&item.id, &decided);
            for check in TOOL_CHECKS.iter().filter(|c| wanted(c)) {
                // the rules have no rule for the match: their answer is the unsafe end for every plan.
                // The core asks it only of a plan a model filled or one that reads the code, and with
                // the rules deciding refuses that plan by design, so there is nothing here to measure
                if judge.is_none() && *check == "args_match_question" {
                    continue;
                }
                let fired = decided.verdicts.get(*check).is_some_and(|v| *v != Verdict::Pass);
                report
                    .checks
                    .entry((*check).to_owned())
                    .or_default()
                    .add(item.expect.fires.iter().any(|f| f == check), fired);
            }
        }
    }
    report
}

async fn ask(
    judge: Option<&dyn Judge>,
    catalogue: &Catalogue,
    stage: &StageBatch,
    rules: &RuleContext,
) -> Result<stages::StageVerdicts, StageError> {
    match judge {
        Some(judge) => stages::decide(judge, catalogue, stage).await,
        None => stages::decide(&stages::rules_for(stage, rules), catalogue, stage).await,
    }
}

// ---------------------------------------------------------------------------------------- the report

fn pct(value: Option<f64>) -> String {
    value.map_or("–".to_owned(), |v| format!("{:.0}%", v * 100.0))
}

/// The report as Markdown.
pub fn render(date: &str, sets: &Sets, reports: &[BackendReport], notes: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Judge evaluation, {date}\n");
    let forbidden = sets
        .questions
        .iter()
        .filter(|q| q.expect.refused_by_a_check())
        .count();
    let _ = writeln!(
        out,
        "Sets: {} questions ({} refused by a check, {} by the policy), {} sentences, {} titles, \
         {} plans. Each item is one request of its stage. The sentence set holds fact ids, not the facts, so \
         `grounded` is rated against ids alone: its row says what the set can show, not what the judge can. Of \
         them {} questions, {} sentences, {} titles and {} plans are held out: written after the thresholds were \
         set, and read as a section of their own, `, held out`, beside the part they were tuned on.\n",
        sets.questions.len(),
        forbidden,
        sets.questions
            .iter()
            .filter(|q| q.expect.refuse && !q.expect.refused_by_a_check())
            .count(),
        sets.sentences.len(),
        sets.titles.len(),
        sets.plans.len(),
        sets.questions.iter().filter(|q| q.held_out).count(),
        sets.sentences.iter().filter(|s| s.held_out).count(),
        sets.titles.iter().filter(|t| t.held_out).count(),
        sets.plans.iter().filter(|p| p.held_out).count(),
    );
    for note in notes {
        let _ = writeln!(out, "> {note}");
    }
    if !notes.is_empty() {
        out.push('\n');
    }
    for report in reports {
        let _ = writeln!(out, "## {}\n", report.name);
        if let Some(why) = &report.skipped {
            let _ = writeln!(out, "Skipped: {why}.\n");
            continue;
        }
        let _ = writeln!(
            out,
            "{} requests, {} errors; latency p50 {} ms, p95 {} ms; cost ${:.6}{}.\n",
            report.requests,
            report.errors,
            report.percentile(0.5).map_or("–".into(), |v| v.to_string()),
            report.percentile(0.95).map_or("–".into(), |v| v.to_string()),
            report.cost_usd,
            if report.unknown_cost > 0 {
                format!(" ({} requests reported no cost)", report.unknown_cost)
            } else {
                String::new()
            }
        );
        let _ = writeln!(
            out,
            "| Check | n | Accuracy | Precision | Recall | TP | FP | FN | TN |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
        for (check, c) in &report.checks {
            let _ = writeln!(
                out,
                "| `{check}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                c.n(),
                pct(c.accuracy()),
                pct(c.precision()),
                pct(c.recall()),
                c.tp,
                c.fp,
                c.fn_,
                c.tn
            );
        }
        if !report.refusals.is_empty() {
            let _ = writeln!(out, "\n| Language | Refusal recall | False refusals |");
            let _ = writeln!(out, "|---|---|---|");
            for (lang, [forbidden, refused, allowed, wrong]) in &report.refusals {
                let _ = writeln!(out, "| {lang} | {refused}/{forbidden} | {wrong}/{allowed} |");
            }
        }
        if !report.classes.is_empty() {
            let mut columns: Vec<String> = stages::CLASSES.iter().map(|(_, n, _)| (*n).to_owned()).collect();
            columns.push("clarify".to_owned());
            let _ = writeln!(out, "\nClass, expected (rows) × picked (columns):\n");
            let _ = writeln!(out, "| | {} |", columns.join(" | "));
            let _ = writeln!(out, "|---|{}", "---|".repeat(columns.len()));
            for (expected, picked) in &report.classes {
                let cells: Vec<String> = columns
                    .iter()
                    .map(|c| picked.get(c).map_or(String::new(), u32::to_string))
                    .collect();
                let _ = writeln!(out, "| {expected} | {} |", cells.join(" | "));
            }
        }
        if !report.failed.is_empty() {
            let _ = writeln!(out, "\nFailed items (counted as errors, never as a pass):\n");
            for (id, why) in report.failed.iter().take(20) {
                let _ = writeln!(out, "- {id}: {why}");
            }
        }
        out.push('\n');
    }
    out
}
