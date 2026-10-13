//! The four stages of a question as judge requests: each stage's context becomes **one** batch of
//! every check the catalogue asks a judge at that stage, and its answers become verdicts. A judge
//! error or a missing answer is an error of the whole stage, never a pass.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use mercury_schema::config::{CheckConfig, CheckStage, CheckType};
use mercury_schema::redact::{TextRejection, validate_safe_text};
use mercury_schema::{QuestionClass, ToolName, Verdict};
use regex::Regex;
use serde_json::{Value, json};

use crate::catalogue::{Catalogue, VerdictError, verdict};
use crate::rules::Test;
use crate::{Answer, Batch, Judge, JudgeError, Outcome, Polarity, Question, Rule, RulesJudge};

/// What Mercury is for, the `policy` every stage's state carries.
pub const POLICY: &str = "Mercury answers internal questions about a project: its progress, tests, blockers, build \
     health and recent changes, and anything else about the project's work, such as how something works or an \
     estimate of a change. It never shows source code, file paths, secrets, evaluations of individual people, \
     commitments to clients, or confidential information: contracts, prices, salaries, personal data, a client's \
     internal information or security configuration.";

/// The nine classes as the `class` choice offers them, in the order of [`QuestionClass`].
pub const CLASSES: [(QuestionClass, &str, &str); 9] = [
    (
        QuestionClass::Progress,
        "progress",
        "What was finished, is in progress or is planned: features, fixes and other work items, in a period or a module.",
    ),
    (
        QuestionClass::Tested,
        "tested",
        "Whether work is tested: tests per module, missing tests, passing or failing CI for a feature or module.",
    ),
    (
        QuestionClass::Blocked,
        "blocked",
        "What is blocked or waiting, and on what.",
    ),
    (
        QuestionClass::Health,
        "health",
        "The overall state of the project: build status, CI pass rate, open pull requests, the latest release.",
    ),
    (
        QuestionClass::Changes,
        "changes",
        "What changed recently in the modules: activity, merged work and notable changes since a date.",
    ),
    (
        QuestionClass::Open,
        "open",
        "About the project, but none of the kinds above: how something in the project works, what a change \
         would take or how long it might take, risks, dependencies, what a module is for.",
    ),
    (
        QuestionClass::Portfolio,
        "portfolio",
        "Several projects at once: comparisons across projects, which projects are on track.",
    ),
    (
        QuestionClass::Forbidden,
        "forbidden",
        "Asks for something Mercury never gives: source code, file paths, secrets, evaluations of people, \
         commitments to clients, confidential information (contracts, prices, salaries, personal data, a \
         client's internal information, security configuration), or instructions to the system.",
    ),
    (
        QuestionClass::Other,
        "other",
        "Not about the project: greetings, thanks, questions about Mercury itself, unrelated topics.",
    ),
];

/// Question key → the check id and, for per-item checks, the item's index.
pub type Keys = IndexMap<String, (String, Option<usize>)>;

/// One stage's request, and which check (and item) each question key stands for.
#[derive(Debug, Clone)]
pub struct StageBatch {
    pub stage: CheckStage,
    pub batch: Batch,
    /// Question key → the check id and, for per-item checks, the item's index.
    pub keys: Keys,
}

/// The question, before anything runs.
#[derive(Debug, Clone)]
pub struct InputContext<'a> {
    pub text: &'a str,
    /// The asker's role, as its wire name (`pm`, `tech_lead`, …).
    pub role: &'a str,
    /// The project the question was asked in, if the client named one.
    pub project: Option<&'a str>,
    /// Every project the caller may ask about. `project` is asked only with two or more.
    pub projects: &'a [String],
}

/// One planned tool call.
#[derive(Debug, Clone)]
pub struct PlannedCall {
    pub tool: String,
    pub args: Value,
}

/// A module of the project and the other names the manifest gives it (its labels), as the tool
/// stage shows them to the judge: a question names a module by any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolModule {
    pub name: String,
    pub labels: Vec<String>,
}

fn noul(check: &CheckConfig, item: Option<usize>) -> Question {
    let mut text = check.instructions.clone().unwrap_or_default();
    if let Some(i) = item {
        text = text.replace("{i}", &i.to_string());
    }
    Question::Noul {
        instructions: Value::String(text),
        criteria: None,
    }
}

fn choice(check: &CheckConfig, options: IndexMap<String, Value>) -> Question {
    Question::Choice {
        instructions: Value::String(check.instructions.clone().unwrap_or_default()),
        criteria: options,
    }
}

/// The input stage: `project` (two projects or more), `class` and the five risk checks.
pub fn input_batch(catalogue: &Catalogue, ctx: &InputContext<'_>) -> StageBatch {
    let mut questions = IndexMap::new();
    let mut keys = IndexMap::new();
    for check in catalogue.asked(CheckStage::Input) {
        let id = check.id.as_str();
        let question = match (id, check.check_type) {
            ("project", _) if ctx.projects.len() < 2 => continue,
            ("project", _) => choice(
                check,
                ctx.projects.iter().map(|p| (p.clone(), Value::Null)).collect(),
            ),
            ("class", _) => choice(
                check,
                CLASSES
                    .iter()
                    .map(|(_, name, description)| {
                        ((*name).to_owned(), Value::String((*description).to_owned()))
                    })
                    .collect(),
            ),
            (_, CheckType::Noul) => noul(check, None),
            _ => continue,
        };
        questions.insert(id.to_owned(), question);
        keys.insert(id.to_owned(), (id.to_owned(), None));
    }
    StageBatch {
        stage: CheckStage::Input,
        batch: Batch {
            state: json!({
                "policy": POLICY,
                "question": { "text": ctx.text, "role": ctx.role, "project": ctx.project },
            }),
            questions,
        },
        keys,
    }
}

/// The tool stage: the model-answered checks about the planned calls. `scope` and `deny_class` are
/// code's and never sent. `modules` are the project's modules with their labels, as `describe` lists
/// them: what a module in an argument has to be matched against, since a person writes "billing" for
/// the module Invoicing. A list that is empty (a project with no module the models may read) is left
/// out of the state.
pub fn tool_batch(
    catalogue: &Catalogue,
    question: &str,
    calls: &[PlannedCall],
    modules: &[ToolModule],
) -> StageBatch {
    let shown: Vec<Value> = calls.iter().map(args_shown_to_the_judge).collect();
    let mut values = Vec::new();
    for args in &shown {
        collect_strings(args, &mut values);
    }
    let (questions, keys) = per_stage(catalogue, CheckStage::Tool, 1, false);
    let mut state = json!({
        "policy": POLICY,
        "question": { "text": question },
        "plan": {
            "calls": calls
                .iter()
                .zip(&shown)
                .map(|(c, args)| json!({ "tool": c.tool, "args": args }))
                .collect::<Vec<_>>(),
            "args_text": values.join(" "),
        },
    });
    if !modules.is_empty() {
        state["modules"] = modules
            .iter()
            .map(|module| json!({ "name": module.name, "labels": module.labels }))
            .collect();
    }
    StageBatch {
        stage: CheckStage::Tool,
        batch: Batch { state, questions },
        keys,
    }
}

/// The data stage: each text value a probe returned, `f{i}.<check>`.
pub fn data_batch(catalogue: &Catalogue, fields: &[String]) -> StageBatch {
    let (questions, keys) = per_stage(catalogue, CheckStage::Data, fields.len(), true);
    StageBatch {
        stage: CheckStage::Data,
        batch: Batch {
            state: json!({
                "policy": POLICY,
                "fields": fields.iter().map(|f| json!({ "text": f })).collect::<Vec<_>>(),
            }),
            questions,
        },
        keys,
    }
}

/// The output stage: every sentence × every output check, `s{i}.<check>`; `facts` is what the
/// answer may rest on.
pub fn output_batch(catalogue: &Catalogue, sentences: &[String], facts: &Value) -> StageBatch {
    let (questions, keys) = per_stage(catalogue, CheckStage::Output, sentences.len(), true);
    StageBatch {
        stage: CheckStage::Output,
        batch: Batch {
            state: json!({
                "policy": POLICY,
                "facts": facts,
                "answer": { "sentences": sentences },
            }),
            questions,
        },
        keys,
    }
}

/// Every model-answered check of a stage, once or per item.
fn per_stage(
    catalogue: &Catalogue,
    stage: CheckStage,
    items: usize,
    per_item: bool,
) -> (IndexMap<String, Question>, Keys) {
    let mut questions = IndexMap::new();
    let mut keys = IndexMap::new();
    let prefix = if stage == CheckStage::Data { "f" } else { "s" };
    for i in 0..items {
        for check in catalogue.asked(stage) {
            let id = check.id.as_str();
            let (key, item) = if per_item {
                (format!("{prefix}{i}.{id}"), Some(i))
            } else {
                (id.to_owned(), None)
            };
            questions.insert(key.clone(), noul(check, item));
            keys.insert(key, (id.to_owned(), item));
        }
    }
    (questions, keys)
}

/// A call's arguments as the tool stage shows them, in `plan.calls` and in `args_text`. The code
/// reader's `question` is not shown: the core makes it from the person's question by redaction alone,
/// so no model wrote it, and the input stage already read that question for instructions; a check
/// that wants only names, dates and numbers would refuse any sentence. The call itself is shown, so
/// the plan still says the code is read.
fn args_shown_to_the_judge(call: &PlannedCall) -> Value {
    let mut args = call.args.clone();
    if call.tool == ToolName::AskCode.as_str()
        && let Value::Object(map) = &mut args
    {
        map.remove("question");
    }
    args
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// A stage decided: the verdict per question key, the `class` pick (input stage), and the outcome.
#[derive(Debug, Clone)]
pub struct StageVerdicts {
    pub verdicts: IndexMap<String, Verdict>,
    pub class: Option<String>,
    pub outcome: Outcome,
}

impl StageVerdicts {
    /// The strictest verdict of the stage.
    pub fn strictest(&self) -> Verdict {
        self.verdicts.values().copied().max().unwrap_or(Verdict::Pass)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StageError {
    #[error(transparent)]
    Judge(#[from] JudgeError),
    #[error(transparent)]
    Verdict(#[from] VerdictError),
    #[error("the catalogue has no check `{0}`")]
    UnknownCheck(String),
}

/// Asks the judge once, and turns every answer into a verdict. Any error, a missing answer
/// included, is the stage's error: never a pass.
pub async fn decide(
    judge: &dyn Judge,
    catalogue: &Catalogue,
    stage: &StageBatch,
) -> Result<StageVerdicts, StageError> {
    let outcome = judge.decide(&stage.batch).await?;
    verdicts(catalogue, stage, outcome)
}

/// The verdicts of an outcome a judge already gave.
pub fn verdicts(
    catalogue: &Catalogue,
    stage: &StageBatch,
    outcome: Outcome,
) -> Result<StageVerdicts, StageError> {
    let mut verdicts = IndexMap::with_capacity(stage.keys.len());
    let mut class = None;
    for (key, (id, _)) in &stage.keys {
        let check = catalogue
            .get(id)
            .ok_or_else(|| StageError::UnknownCheck(id.clone()))?;
        let answer = outcome.answers.get(key);
        verdicts.insert(key.clone(), verdict(check, answer, outcome.mode)?);
        if let (Some(Answer::Choice { choice, .. }), "class") = (answer, id.as_str()) {
            class = Some(choice.clone());
        }
    }
    Ok(StageVerdicts {
        verdicts,
        class,
        outcome,
    })
}

// ---------------------------------------------------------------------------------------------
// The rules per check

fn re(pattern: &str) -> Regex {
    #[allow(
        clippy::expect_used,
        reason = "the patterns are literals of this module, covered by its tests"
    )]
    Regex::new(pattern).expect("literal pattern")
}

/// A secret written out in words; the SafeText rules look for the key=value forms.
static SECRET_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(password|passwd|passphrase|api[ _-]?key|access token|secret)\s*(is|:|=|to)?\s+\S*\d\S*")
});

/// A fact cited the way the composer cites it: `[f_7Qx2]`.
static CITATION: LazyLock<Regex> = LazyLock::new(|| re(r"\s*\[f_[A-Za-z0-9]{4,16}\]"));

/// Characters that control, hide or reorder text, as SafeText refuses them: every control and
/// format character, every default-ignorable code point, the variation selectors.
static INVISIBLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"[\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}\p{Variation_Selector}]"));

/// The text without its invisible characters: what a person reads of it, so a name split by a
/// zero-width space is the name.
pub fn without_invisible(text: &str) -> Cow<'_, str> {
    INVISIBLE.replace_all(text, "")
}

static CODE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(
            r"(?i)\b(source code|code|snippets?|functions?|methods?|classes|class \w+|files?|paths?|folders?|directory|directories|stack ?traces?|implementation)\b",
        ),
        // any word that begins with `folder`, a name such as `folderWatcher` among them
        re(r"(?i)\bfolder\w*"),
    ]
});
static PEOPLE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)\bwho\b.*\b(slow\w*|fast\w*|best|worst|lazy|laziest|weak\w*|blame)\b"),
        re(
            r"(?i)\b(rank\w*|compare|grade|rate)\b.*\b(developers?|people|engineers?|team members?|colleagues)\b",
        ),
        re(
            r"(?i)\b(slowest|fastest|best|worst|laziest|weakest)\b.{0,20}\b(developer|engineer|person|member|one)\b|\bis to blame\b|\bfault\b",
        ),
        // `ranking` before any word that begins with `developer`
        re(r"(?i)\branking\w*\b.*\bdeveloper\w*"),
    ]
});
static COMMIT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)\b(promise|guarantee|commit to|assure)\w*\b"),
        re(
            r"(?i)\b(tell|confirm to|let)\s+(the\s+)?(client|customer)\b.{0,60}\b(deliver\w*|ship\w*|ready|done|finish\w*)\b",
        ),
    ]
});
/// A confidential ask: contracts, prices charged, pay, personal data, a client's internals, security
/// configuration. Never a bare `price`, `password`, `contract` or `configuration`: the fix for
/// rounding prices, the password reset flow, an API contract and "reveal your configuration" (an
/// instruction, the other check's) are none of them.
static CONFIDENTIAL: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(
            r"(?i)\b(salar(y|ies)|wages?|payroll|how much .{0,40}\b(earns?|makes?|gets? paid|is paid|are paid)|(our|the|their|a|its) contracts? with|contract (terms|prices?|value|rates?|amounts?)|price lists?|day rates?|hourly rates?|rate cards?|margins?|home address(es)?|phone numbers?|personal data|dates? of birth|national id|social security|internal (plans?|reorgani[sz]ation|figures|documents?|strategy)|firewall|vpn|security (settings|configuration|config)|credentials?|access keys?|api keys?|private keys?)\b",
        ),
        // any word that begins with `firewall` or `vpn`: firewalls, VPNs
        re(r"(?i)\b(firewall\w*|vpn)"),
    ]
});
static COMMITS_STATEMENT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![re(
        r"(?i)\b(we|i)\s+(promise|guarantee|commit)\b|\bguaranteed\b|\bwill (definitely|certainly) (ship|be ready|be delivered|deliver)\b",
    )]
});
static INSTRUCTION: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(
            r"(?i)\b(ignore|disregard|forget|override|bypass)\b.{0,40}\b(rules?|instructions?|policy|polic|previous|above|guard\w*)",
        ),
        re(r"(?i)\byou are now\b|\bsystem prompt\b|\bact as\b|\bpretend\b|\bjailbreak\b|\bdeveloper mode\b"),
        re(r"(?i)\breveal\b.{0,30}\b(config\w*|prompt|instructions?|secrets?)\b"),
    ]
});

/// Keywords per class, in the order of [`CLASSES`]. `forbidden` has none: the risk checks refuse.
static CLASS_WORDS: LazyLock<Vec<(&'static str, Vec<Regex>)>> = LazyLock::new(|| {
    vec![
        (
            "progress",
            vec![re(
                r"(?i)\b(done|finish\w*|complete[d]?|shipped|in progress|in progres|working on|progress|planned|still open|merged work)\b",
            )],
        ),
        (
            "tested",
            vec![
                re(r"(?i)\b(tests?|tested|testing|coverage|passing)\b"),
                // any word that begins with `test`: testers, testable
                re(r"(?i)\btest\w*"),
            ],
        ),
        ("blocked", vec![re(r"(?i)\b(block\w*|waiting|stuck)\b")]),
        (
            "health",
            vec![
                re(r"(?i)\b(health\w*|build|builds|pipeline|ci pass rate|release)\b"),
                // any word that begins with `build`: building, buildable
                re(r"(?i)\bbuild\w*"),
            ],
        ),
        (
            "changes",
            vec![re(r"(?i)\b(changed?|changes|since|what's new|activity|merged)\b")],
        ),
        // how something works, what a change would take, risks: no `when`, so "when will it be done"
        // stays `progress`
        (
            "open",
            vec![re(
                r"(?i)\b(how long|how much (work|time|effort)|how many (days|weeks|hours)|estimat\w*|roughly|how (hard|difficult|big|complex|risky)|how does .{0,40}\bwork|why|what would it take|would it take|risks?|depends? on|supports? (more|multiple|several|other)|what is .{0,30}\bfor|who uses|cope with|scale|maintain\w*|learn\w*)\b",
            )],
        ),
        (
            "portfolio",
            vec![re(r"(?i)\b(projects|portfolio|across (the|our) projects)\b")],
        ),
        ("forbidden", vec![]),
        (
            "other",
            vec![re(
                r"(?i)\b(thanks|thank you|hello|hi there|bye|who are you|what can you do|help me with)\b",
            )],
        ),
    ]
});

/// Whether a sentence breaks one of these SafeText rules. Each word is judged alone (a name, a
/// path or a URL is one word), and so is each run of words short enough for the length rule, so a
/// rule the schema checks first can't hide another. The length and the invisible-character rules
/// come before every other, so a word too long for SafeText, or one holding an invisible character,
/// is a hit whatever the kinds: either would hide what they look for.
fn breaks(text: &str, kinds: &[TextRejection]) -> bool {
    // a cited fact id is the answer's own reference, not a name from the code
    let text = CITATION.replace_all(text, "");
    let hit = |piece: &str| {
        matches!(validate_safe_text(piece), Err(kind) if kinds.contains(&kind)
            || matches!(kind, TextRejection::TooLong | TextRejection::ControlCharacter))
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.iter().any(|word| hit(word)) {
        return true;
    }
    let mut piece = String::new();
    for word in &words {
        if !piece.is_empty() && piece.chars().count() + word.chars().count() >= 100 {
            if hit(&piece) {
                return true;
            }
            piece.clear();
        }
        if !piece.is_empty() {
            piece.push(' ');
        }
        piece.push_str(word);
    }
    !piece.is_empty() && hit(&piece)
}

/// What the rules need beyond the patterns: the client names to look for, the fact ids an answer
/// may cite, and the caller's projects.
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    pub client_patterns: Vec<Regex>,
    pub fact_ids: Vec<String>,
    pub projects: Vec<String>,
}

/// The rules judge for one stage's batch: each question key answered by its check's rule.
pub fn rules_for(stage: &StageBatch, ctx: &RuleContext) -> RulesJudge {
    let mut rules = HashMap::new();
    for (key, (id, item)) in &stage.keys {
        let field = match (stage.stage, item) {
            (CheckStage::Input, _) => "/question/text".to_owned(),
            (CheckStage::Tool, _) => "/plan/args_text".to_owned(),
            (CheckStage::Data, Some(i)) => format!("/fields/{i}/text"),
            (CheckStage::Output, Some(i)) => format!("/answer/sentences/{i}"),
            _ => String::new(),
        };
        rules.insert(key.clone(), rule(id, field, ctx));
    }
    RulesJudge::new(rules)
}

fn patterns(field: String, polarity: Polarity, list: &[Regex]) -> Rule {
    Rule::Noul {
        polarity,
        field,
        patterns: list.to_vec(),
    }
}

fn predicate(field: String, polarity: Polarity, test: impl Fn(&str) -> bool + Send + Sync + 'static) -> Rule {
    Rule::Predicate {
        polarity,
        field,
        test: Test::new(test),
    }
}

fn rule(id: &str, field: String, ctx: &RuleContext) -> Rule {
    use Polarity::{Legit, Risk};
    match id {
        "class" => Rule::Choice {
            field,
            options: CLASS_WORDS
                .iter()
                .map(|(name, words)| ((*name).to_owned(), words.clone()))
                .collect(),
        },
        "project" => Rule::Choice {
            field,
            options: ctx
                .projects
                .iter()
                .map(|p| (p.clone(), vec![re(&format!(r"(?i)\b{}\b", regex::escape(p)))]))
                .collect(),
        },
        "asks_for_code" => patterns(field, Risk, &CODE),
        "asks_to_evaluate_people" | "sentence_evaluates_people" => patterns(field, Risk, &PEOPLE),
        "asks_for_client_commitment" => patterns(field, Risk, &COMMIT),
        "asks_for_confidential" => patterns(field, Risk, &CONFIDENTIAL),
        "sentence_commits_to_client" => patterns(field, Risk, &COMMITS_STATEMENT),
        "contains_agent_instruction" | "field_contains_agent_instruction" => {
            patterns(field, Risk, &INSTRUCTION)
        }
        // every string argument, `ask_code`'s question among them: words pass, an instruction doesn't
        "args_free_of_instructions" => {
            predicate(field, Legit, |text| !INSTRUCTION.iter().any(|p| p.is_match(text)))
        }
        "sentence_contains_code_or_identifier" => predicate(field, Risk, |text| {
            breaks(
                text,
                &[
                    TextRejection::Identifier,
                    TextRejection::Backtick,
                    TextRejection::CodePunctuation,
                ],
            )
        }),
        "sentence_contains_path_or_secret" => predicate(field, Risk, |text| {
            SECRET_WORDS.is_match(text)
                || breaks(
                    text,
                    &[
                        TextRejection::Path,
                        TextRejection::FileName,
                        TextRejection::Url,
                        TextRejection::Secret,
                        TextRejection::Email,
                    ],
                )
        }),
        "sentence_mentions_client" => {
            // no client on the list is nothing to look for: a miss, which is an answer
            let clients = ctx.client_patterns.clone();
            predicate(field, Risk, move |text| {
                let text = without_invisible(text);
                clients.iter().any(|p| p.is_match(&text))
            })
        }
        "grounded" => {
            let ids = ctx.fact_ids.clone();
            predicate(field, Legit, move |text| {
                ids.iter().any(|id| text.contains(id.as_str()))
            })
        }
        // `args_match_question` and anything unknown: the rules can't tell, so the unsafe end. For
        // `ask_code` that means no question reaches the code reader that no judge read.
        _ => Rule::Undecided {
            polarity: match id {
                "args_match_question" => Legit,
                _ => Risk,
            },
        },
    }
}
