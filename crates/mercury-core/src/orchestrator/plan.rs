//! The plan: a fixed table picks the tools for a class, and the planner model may only fill
//! `since`, `module` and `limit`. Its answer is checked value by value against today and the
//! probe's modules; anything else leaves the tool's default. It never names a tool.

use std::sync::LazyLock;

use chrono::{NaiveDate, TimeDelta};
use mercury_schema::events::ToolCall;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::redact::redacted_question;
use mercury_schema::tools::Limit;
use mercury_schema::{AccessMode, ModuleName, QuestionClass, RedactedQuestion, Redactor, ToolName};
use regex::Regex;
use serde_json::{Map, Value, json};

/// How far back `since` may reach.
pub const OLDEST_SINCE: TimeDelta = TimeDelta::days(366);
/// `changes` needs a start; without one from the planner it is a week ago.
pub const DEFAULT_CHANGES_WINDOW: TimeDelta = TimeDelta::days(7);
/// How far back `changes` reaches: the probe keeps the 90 days before its metadata's time and
/// refuses a date whose midnight is older rather than count part of the period, so an older start
/// is moved up to 89 days before today, whose midnight is inside that window for any metadata
/// fetched up to the end of today.
pub const CHANGES_REACH: TimeDelta = TimeDelta::days(89);

/// The tools a class calls with the probe it has. A model never chooses them, whatever the question
/// says.
///
/// - Each of the five kinds calls its own tool, except that `changes` calls `progress` until the
///   probe serves `changes`, so a question about what changed is answered from the work updated
///   since its date instead of failing.
/// - An `open` question calls every fact tool the probe serves (`describe` is the planner's and the
///   composer's, not a lookup), in the order of [`ToolName::ALL`], and then `ask_code`, the code
///   reader, when the probe's mode is `code_model` and it serves the tool.
/// - No other class calls anything, and none but `open` ever calls `ask_code`.
pub fn tools_for(class: QuestionClass, served: &[ToolName], access: AccessMode) -> Vec<ToolName> {
    match class {
        QuestionClass::Progress => vec![ToolName::Progress],
        QuestionClass::Tested => vec![ToolName::Tested],
        QuestionClass::Blocked => vec![ToolName::Blocked],
        QuestionClass::Health => vec![ToolName::Health],
        QuestionClass::Changes if served.contains(&ToolName::Changes) => vec![ToolName::Changes],
        QuestionClass::Changes => vec![ToolName::Progress],
        QuestionClass::Open => {
            let mut tools: Vec<ToolName> = ToolName::ALL
                .into_iter()
                .filter(|tool| *tool != ToolName::Describe && served.contains(tool))
                .collect();
            if access == AccessMode::CodeModel && served.contains(&ToolName::AskCode) {
                tools.push(ToolName::AskCode);
            }
            tools
        }
        QuestionClass::Portfolio | QuestionClass::Forbidden | QuestionClass::Other => Vec::new(),
    }
}

/// The question as `ask_code` takes it: redacted with the project's rules, whitespace read as one
/// space, on one line of at most 1,000 characters. `None` for a question that doesn't fit once
/// redacted: it is never shortened, so the reader isn't asked.
pub fn code_question(text: &str, redactor: &Redactor) -> Option<RedactedQuestion> {
    redacted_question(text, redactor).ok()
}

/// What the planner filled, with the start `changes` needs when `progress` stands in for it.
pub fn filled_for(class: QuestionClass, tools: &[ToolName], mut filled: Filled, today: NaiveDate) -> Filled {
    if class == QuestionClass::Changes && tools == [ToolName::Progress] && filled.since.is_none() {
        filled.since = Some(today - DEFAULT_CHANGES_WINDOW);
    }
    filled
}

/// The modules a probe's replies may name, as its `describe` lists them: the manifest's modules
/// in order, then `Other`.
pub fn modules(manifest: &ProbeManifest) -> Vec<ModuleName> {
    let mut modules: Vec<ModuleName> = Vec::new();
    for module in manifest.modules.keys().chain(manifest.module_labels.keys()) {
        if !modules.contains(module) {
            modules.push(module.clone());
        }
    }
    modules.push(ModuleName::other());
    modules
}

/// Each module with the other names the manifest gives it (its labels), for the planner and for
/// reading a module out of the question.
pub fn names(manifest: &ProbeManifest) -> Vec<(ModuleName, Vec<String>)> {
    modules(manifest)
        .into_iter()
        .map(|module| {
            let labels = manifest
                .module_labels
                .get(&module)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|label| !label.eq_ignore_ascii_case(module.as_str()))
                .collect();
            (module, labels)
        })
        .collect()
}

/// A label's shape: at most 40 characters, a letter first, then letters, digits, spaces and
/// `& + - / : _ .` (a module name's shape, widened for labels such as `ci/cd` or `area:billing`).
static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(
        clippy::expect_used,
        reason = "a literal pattern, covered by the orchestrator's tests"
    )]
    Regex::new(r"^\p{L}[\p{L}\p{N} &+/:_.-]{0,39}$").expect("literal pattern")
});

/// The names a model may read: without a module or a label the project's redaction would change,
/// since a label can name a client, and without a label that isn't shaped like one, since the
/// repository writes them and one may hold an instruction. A label left out still stands for its
/// module in code.
pub fn shown_to_models(
    names: &[(ModuleName, Vec<String>)],
    redactor: &Redactor,
) -> Vec<(ModuleName, Vec<String>)> {
    let kept = |text: &str| redactor.apply(text) == text;
    names
        .iter()
        .filter(|(module, _)| kept(module.as_str()))
        .map(|(module, labels)| {
            let labels = labels
                .iter()
                .filter(|label| LABEL.is_match(label) && kept(label))
                .cloned()
                .collect();
            (module.clone(), labels)
        })
        .collect()
}

/// The module a name or a label stands for, in any case.
fn module_of(name: &str, names: &[(ModuleName, Vec<String>)]) -> Option<ModuleName> {
    let name = name.trim();
    names
        .iter()
        .find(|(module, labels)| {
            module.as_str().eq_ignore_ascii_case(name) || labels.iter().any(|l| l.eq_ignore_ascii_case(name))
        })
        .map(|(module, _)| module.clone())
}

/// Whether the text names `name` as a whole word, in any case.
pub fn mentions(text: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let pattern = format!(
        r"(?i)(^|[^\p{{L}}\p{{N}}]){}($|[^\p{{L}}\p{{N}}])",
        regex::escape(name)
    );
    regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(text))
}

/// The one module the question names, by its name or a label; `Other` is never read this way.
pub fn named_module(question: &str, names: &[(ModuleName, Vec<String>)]) -> Option<ModuleName> {
    let named: Vec<&ModuleName> = names
        .iter()
        .filter(|(module, _)| module.as_str() != ModuleName::OTHER)
        .filter(|(module, labels)| {
            std::iter::once(module.as_str())
                .chain(labels.iter().map(String::as_str))
                .any(|name| mentions(question, name))
        })
        .map(|(module, _)| module)
        .collect();
    match named.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// What the planner filled; `None` leaves the tool's default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filled {
    pub since: Option<NaiveDate>,
    pub module: Option<ModuleName>,
    pub limit: Option<Limit>,
}

impl Filled {
    pub fn is_empty(&self) -> bool {
        *self == Filled::default()
    }
}

/// The planner's prompt, from its file: `{today}`, `{modules}` (each with its labels) and
/// `{question}` filled in, the question last.
pub fn prompt(
    template: &str,
    today: NaiveDate,
    names: &[(ModuleName, Vec<String>)],
    question: &str,
) -> String {
    let listed: Vec<String> = names
        .iter()
        .map(|(module, labels)| {
            if labels.is_empty() {
                module.to_string()
            } else {
                format!("{module} (also {})", labels.join(", "))
            }
        })
        .collect();
    template
        .replace("{today}", &today.format("%Y-%m-%d").to_string())
        .replace("{modules}", &listed.join(", "))
        .replace("{question}", question)
}

/// The planner's answer, checked. `None` when it isn't one JSON object; a value that is missing,
/// `null` or out of bounds leaves that one value to the default. A module may come as one of its
/// labels.
pub fn parse(content: &str, today: NaiveDate, names: &[(ModuleName, Vec<String>)]) -> Option<Filled> {
    let value: Value = serde_json::from_str(content.trim()).ok()?;
    let object = value.as_object()?;
    let since = object
        .get("since")
        .and_then(Value::as_str)
        .and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
        .filter(|date| *date <= today && *date >= today - OLDEST_SINCE);
    let module = object
        .get("module")
        .and_then(Value::as_str)
        .and_then(|name| module_of(name, names));
    let limit = object
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .and_then(Limit::new);
    Some(Filled { since, module, limit })
}

/// The calls for the class's tools with what the planner filled, each typed, so an argument a
/// tool doesn't take can't get through. `question` is `ask_code`'s one argument; without it that
/// call doesn't parse and is left out.
pub fn calls(
    tools: &[ToolName],
    filled: &Filled,
    today: NaiveDate,
    question: Option<&RedactedQuestion>,
) -> Vec<ToolCall> {
    tools
        .iter()
        .filter_map(|tool| {
            let mut args = Map::new();
            let mut put = |key: &str, value: Value| {
                args.insert(key.to_owned(), value);
            };
            let since = filled.since.map(|d| json!(d.format("%Y-%m-%d").to_string()));
            let module = filled.module.as_ref().map(|m| json!(m.as_str()));
            match tool {
                ToolName::Progress => {
                    if let Some(since) = since {
                        put("since", since);
                    }
                    if let Some(module) = module {
                        put("module", module);
                    }
                    if let Some(limit) = filled.limit {
                        put("limit", json!(limit.get()));
                    }
                }
                ToolName::Tested => {
                    if let Some(module) = module {
                        put("module", module);
                    }
                }
                ToolName::Blocked => {
                    if let Some(since) = since {
                        put("since", since);
                    }
                }
                ToolName::Changes => {
                    let start = filled
                        .since
                        .unwrap_or(today - DEFAULT_CHANGES_WINDOW)
                        .max(today - CHANGES_REACH);
                    put("since", json!(start.format("%Y-%m-%d").to_string()));
                    if let Some(module) = module {
                        put("module", module);
                    }
                }
                ToolName::AskCode => {
                    if let Some(question) = question {
                        put("question", json!(question.as_str()));
                    }
                }
                ToolName::Health | ToolName::Describe => {}
            }
            serde_json::from_value(json!({ "tool": tool.as_str(), "args": args })).ok()
        })
        .collect()
}

/// A call's arguments as the probe takes them.
pub fn arguments(call: &ToolCall) -> Value {
    serde_json::to_value(call)
        .ok()
        .and_then(|mut value| value.get_mut("args").map(Value::take))
        .unwrap_or_else(|| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn modules() -> Vec<(ModuleName, Vec<String>)> {
        vec![
            (
                ModuleName::parse("Invoicing").unwrap(),
                vec!["billing".to_owned()],
            ),
            (ModuleName::parse("Export").unwrap(), vec![]),
            (ModuleName::other(), vec![]),
        ]
    }

    /// The tools of a class on a probe that serves all six tools, in `code` mode.
    fn tools(class: QuestionClass) -> Vec<ToolName> {
        tools_for(class, &ToolName::ALL, AccessMode::Code)
    }

    #[test]
    fn each_class_has_its_fixed_tools_and_no_model_adds_one() {
        assert_eq!(tools(QuestionClass::Progress), [ToolName::Progress]);
        assert_eq!(tools(QuestionClass::Changes), [ToolName::Changes]);
        for class in [
            QuestionClass::Portfolio,
            QuestionClass::Forbidden,
            QuestionClass::Other,
        ] {
            assert!(tools(class).is_empty(), "{class:?}");
        }
        let filled = parse(
            r#"{"tool": "read_file", "path": "src", "since": null}"#,
            date("2026-10-02"),
            &modules(),
        );
        assert_eq!(
            filled,
            Some(Filled::default()),
            "the planner's own keys are ignored"
        );
    }

    #[test]
    fn the_planners_values_are_checked_one_by_one() {
        let today = date("2026-10-02");
        let filled = parse(
            r#"{"since": "2026-09-28", "module": "export", "limit": 20}"#,
            today,
            &modules(),
        )
        .unwrap();
        assert_eq!(filled.since, Some(date("2026-09-28")));
        assert_eq!(filled.module.as_ref().map(ModuleName::as_str), Some("Export"));
        assert_eq!(filled.limit.map(Limit::get), Some(20));

        let wrong = parse(
            r#"{"since": "2026-12-01", "module": "Payroll", "limit": 500}"#,
            today,
            &modules(),
        )
        .unwrap();
        assert!(
            wrong.is_empty(),
            "a future date, an unknown module and a limit over 50"
        );
        assert!(
            parse(r#"{"since": "2020-01-01"}"#, today, &modules())
                .unwrap()
                .since
                .is_none()
        );
        let label = parse(r#"{"module": "Billing"}"#, today, &modules()).unwrap();
        assert_eq!(
            label.module.as_ref().map(ModuleName::as_str),
            Some("Invoicing"),
            "a label stands for its module"
        );
        assert_eq!(parse("not json", today, &modules()), None);
        assert_eq!(parse("[1, 2]", today, &modules()), None);
    }

    #[test]
    fn calls_take_only_their_own_arguments() {
        let today = date("2026-10-02");
        let filled = Filled {
            since: Some(date("2026-09-28")),
            module: ModuleName::parse("Export").ok(),
            limit: Limit::new(10),
        };
        let all = [
            ToolName::Progress,
            ToolName::Tested,
            ToolName::Blocked,
            ToolName::Health,
            ToolName::Changes,
        ];
        let args: Vec<Value> = calls(&all, &filled, today, None).iter().map(arguments).collect();
        assert_eq!(
            args,
            [
                json!({ "since": "2026-09-28", "module": "Export", "limit": 10 }),
                json!({ "module": "Export" }),
                json!({ "since": "2026-09-28" }),
                json!({}),
                json!({ "since": "2026-09-28", "module": "Export" }),
            ]
        );
        let defaults: Vec<Value> = calls(&all, &Filled::default(), today, None)
            .iter()
            .map(arguments)
            .collect();
        assert_eq!(
            defaults[4],
            json!({ "since": "2026-09-25" }),
            "changes needs a start"
        );
        assert!(defaults[..4].iter().all(|args| args == &json!({})));
        assert!(
            calls(&[ToolName::AskCode], &filled, today, None).is_empty(),
            "ask_code is never planned without its question"
        );
    }

    /// The probe refuses a date whose midnight is more than 90 days before its metadata's time, so
    /// an older start is moved up to 89 days before today, never sent to be refused.
    #[test]
    fn an_older_changes_start_is_moved_up_inside_the_probes_window() {
        let today = date("2026-10-02");
        let start = |since: &str| {
            let filled = Filled {
                since: Some(date(since)),
                ..Filled::default()
            };
            arguments(&calls(&[ToolName::Changes], &filled, today, None)[0])["since"].clone()
        };
        assert_eq!(start("2026-03-01"), json!("2026-07-05"), "far back");
        assert_eq!(
            start("2026-07-04"),
            json!("2026-07-05"),
            "90 days back is part of a day for metadata of any time but midnight"
        );
        assert_eq!(start("2026-07-05"), json!("2026-07-05"), "89 days back is kept");
        assert_eq!(start("2026-09-01"), json!("2026-09-01"));
        // inside the 90 days before metadata of the last second of today, the latest there is
        let latest = date("2026-10-03").and_hms_opt(0, 0, 0).unwrap().and_utc() - TimeDelta::seconds(1);
        let first = (today - CHANGES_REACH).and_hms_opt(0, 0, 0).unwrap().and_utc();
        assert!(first >= latest - TimeDelta::days(90));
    }

    #[test]
    fn open_calls_every_fact_tool_the_probe_serves() {
        let open = |served: &[ToolName]| tools_for(QuestionClass::Open, served, AccessMode::Code);
        assert_eq!(
            open(&ToolName::ALL),
            [
                ToolName::Progress,
                ToolName::Tested,
                ToolName::Blocked,
                ToolName::Health,
                ToolName::Changes
            ],
            "describe is no lookup"
        );
        assert_eq!(
            open(&ToolName::ALL[..5]),
            [
                ToolName::Progress,
                ToolName::Tested,
                ToolName::Blocked,
                ToolName::Health
            ],
            "a probe without changes serves four lookups"
        );
        assert!(open(&[]).is_empty());
    }

    #[test]
    fn open_adds_ask_code_only_under_code_model() {
        let seven = ToolName::CODE_MODEL;
        let ask = |served: &[ToolName], access| tools_for(QuestionClass::Open, served, access);
        assert_eq!(
            ask(&seven, AccessMode::CodeModel).last(),
            Some(&ToolName::AskCode),
            "after the five lookups"
        );
        assert_eq!(ask(&seven, AccessMode::CodeModel).len(), 6);
        assert!(
            !ask(&ToolName::ALL, AccessMode::CodeModel).contains(&ToolName::AskCode),
            "code_model without the tool served"
        );
        for access in [AccessMode::Code, AccessMode::Metadata] {
            assert!(
                !ask(&seven, access).contains(&ToolName::AskCode),
                "{access:?} never calls the reader, whatever the probe lists"
            );
        }
    }

    #[test]
    fn no_class_but_open_ever_calls_ask_code() {
        for class in [
            QuestionClass::Progress,
            QuestionClass::Tested,
            QuestionClass::Blocked,
            QuestionClass::Health,
            QuestionClass::Changes,
            QuestionClass::Portfolio,
            QuestionClass::Forbidden,
            QuestionClass::Other,
        ] {
            for access in [AccessMode::Metadata, AccessMode::Code, AccessMode::CodeModel] {
                let tools = tools_for(class, &ToolName::CODE_MODEL, access);
                assert!(!tools.contains(&ToolName::AskCode), "{class:?} in {access:?}");
            }
        }
        // and `changes` calls `progress` on a probe that doesn't serve it, as before
        assert_eq!(
            tools_for(QuestionClass::Changes, &ToolName::ALL[..5], AccessMode::Code),
            [ToolName::Progress]
        );
    }

    #[test]
    fn ask_codes_argument_is_the_redacted_question() {
        use mercury_schema::redact::RedactRule;
        let redactor = Redactor::new(&[RedactRule {
            pattern: r"(?i)\bmegabank\b".to_owned(),
            replace: "the client".to_owned(),
        }])
        .unwrap();
        let question = code_question(
            "How does the
MegaBank export   pick its format?",
            &redactor,
        )
        .unwrap();
        assert_eq!(
            question.as_str(),
            "How does the the client export pick its format?",
            "the client's name redacted, every run of whitespace one space"
        );
        let call = calls(
            &[ToolName::AskCode],
            &Filled::default(),
            date("2026-10-02"),
            Some(&question),
        );
        assert_eq!(call.len(), 1);
        assert_eq!(
            arguments(&call[0]),
            json!({ "question": "How does the the client export pick its format?" })
        );
        // a question over 1,000 characters once redacted is never shortened: no reader call
        let long = "a ".repeat(501);
        assert!(code_question(&long, &redactor).is_none());
        assert!(code_question("a", &redactor).is_some());
        assert!(code_question("   ", &redactor).is_none(), "nothing to ask");
    }

    #[test]
    fn the_prompt_carries_today_the_modules_and_the_question_last() {
        let text = prompt(
            "Today is {today}. MODULES: {modules}.\nQuestion: {question}",
            date("2026-10-02"),
            &modules(),
            "what's done?",
        );
        assert_eq!(
            text,
            "Today is 2026-10-02. MODULES: Invoicing (also billing), Export, Other.\nQuestion: what's done?"
        );
    }

    #[test]
    fn a_question_names_a_module_by_its_name_or_a_label_once() {
        let named = |text: &str| named_module(text, &modules()).map(|m| m.to_string());
        assert_eq!(named("What is done in billing?").as_deref(), Some("Invoicing"));
        assert_eq!(named("Is EXPORT tested?").as_deref(), Some("Export"));
        assert_eq!(named("Export and billing?"), None, "two modules is no one module");
        assert_eq!(
            named("Any other news?"),
            None,
            "Other is never read from a question"
        );
        assert_eq!(named("exported invoices"), None, "a whole word only");
        assert!(mentions("what about invoices? invoice", "invoice"));
        assert!(!mentions("deltas", "delta") && !mentions("anything", " "));
    }
}
