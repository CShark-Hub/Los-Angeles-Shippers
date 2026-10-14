//! The leak oracle's corpus and calls, shared by the probe's own tests and the conformance runner.
//!
//! The corpus is everything of a fixture repository that must never come out of a probe: its paths,
//! its distinctive identifiers and its code lines. The calls are every tool with varied arguments,
//! and the arguments every tool must refuse.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;
use serde_json::{Value, json};

/// CamelCase or camelCase with at least two humps, snake_case, or SCREAMING_SNAKE.
pub(crate) const IDENTIFIER: &str = r"\b(?:[A-Za-z][a-z0-9]+(?:[A-Z][a-z0-9]*)+|[a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)\b";

/// Identifiers shorter than this are common words of the schema, not the fixture's own.
pub(crate) const MIN_IDENTIFIER: usize = 8;

/// Trimmed lines shorter than this say too little to be a leak.
pub(crate) const MIN_LINE: usize = 12;

/// The manifest's path, whose vocabulary is Mercury's own (`display_name`, `code_model`), words the
/// tool schemas name too: its path is in the corpus, its lines are not.
const MANIFEST: &str = ".mercury/probe.yaml";

/// Everything of the fixture that must never come out of a probe: every path (and file name), every
/// identifier of 8+ characters, every trimmed line of 12+ characters. `meta/` is left out: it is
/// the metadata the facts are made from, not the repository's code.
pub fn corpus_of(root: &Path) -> Result<BTreeSet<String>, String> {
    let identifier = Regex::new(IDENTIFIER).map_err(|err| err.to_string())?;
    let mut corpus = BTreeSet::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = fs::read_dir(&folder)
            .map_err(|err| format!("cannot read the fixture folder {}: {err}", folder.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("cannot read the fixture folder: {err}"))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|err| err.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "meta" {
                continue;
            }
            let kind = entry
                .file_type()
                .map_err(|err| format!("cannot read the fixture folder: {err}"))?;
            if kind.is_dir() {
                folders.push(path);
                continue;
            }
            corpus.insert(relative.clone());
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.len() >= MIN_IDENTIFIER
            {
                corpus.insert(name.to_owned());
            }
            if relative == MANIFEST {
                continue;
            }
            // a file that isn't text has no lines to leak
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            for line in text
                .lines()
                .map(str::trim)
                .filter(|l| l.chars().count() >= MIN_LINE)
            {
                corpus.insert(line.to_owned());
            }
            for found in identifier
                .find_iter(&text)
                .filter(|m| m.as_str().len() >= MIN_IDENTIFIER)
            {
                corpus.insert(found.as_str().to_owned());
            }
        }
    }
    Ok(corpus)
}

/// Every string value in a reply, keys left out. A text content that holds JSON is opened.
pub fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(inner @ (Value::Object(_) | Value::Array(_))) => strings(&inner, out),
            _ => out.push(s.clone()),
        },
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// The corpus entries a reply contains, if any.
pub fn leaks(corpus: &BTreeSet<String>, reply: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    strings(reply, &mut texts);
    corpus
        .iter()
        .filter(|needle| texts.iter().any(|t| t.contains(needle.as_str())))
        .cloned()
        .collect()
}

/// Every tool, every module, several dates, the limit's edges. `changes` reaches 90 days back from
/// the fixture metadata's 2026-09-30 12:00, so its first whole day is 2026-07-03, and into the future.
pub fn calls(modules: &[String]) -> Vec<(&'static str, Value)> {
    let dates = [
        "2020-01-01",
        "2026-09-01",
        "2026-09-20",
        "2026-09-28",
        "2026-09-30",
        "2030-01-01",
    ];
    let mut calls = vec![
        ("describe", json!({})),
        ("health", json!({})),
        ("tested", json!({})),
        ("blocked", json!({})),
        ("progress", json!({})),
        ("progress", json!({ "limit": 1 })),
        ("progress", json!({ "limit": 50 })),
    ];
    for module in modules {
        calls.push(("tested", json!({ "module": module })));
        calls.push(("progress", json!({ "module": module })));
        for limit in [1, 3, 50] {
            calls.push(("progress", json!({ "module": module, "limit": limit })));
        }
        for date in dates {
            calls.push(("progress", json!({ "module": module, "since": date })));
        }
    }
    for date in dates {
        calls.push(("progress", json!({ "since": date })));
        calls.push(("progress", json!({ "since": date, "limit": 2 })));
        calls.push(("blocked", json!({ "since": date })));
    }
    let changes_dates = [
        "2026-07-03",
        "2026-09-01",
        "2026-09-20",
        "2026-09-28",
        "2026-09-30",
        "2030-01-01",
    ];
    for date in changes_dates {
        calls.push(("changes", json!({ "since": date })));
        for module in modules {
            calls.push(("changes", json!({ "since": date, "module": module })));
        }
    }
    calls
}

/// The calls to `ask_code`, which a probe in `code_model` serves: a question about how something is
/// built and one about how a module works, one that asks for the code itself, and one of the longest
/// length a question may have.
pub fn ask_code_calls() -> Vec<(&'static str, Value)> {
    // "export " 142 times is 994 characters, and six more make 1,000 with no space at the end
    let longest = format!("{}abcdef", "export ".repeat(142));
    vec![
        ("ask_code", json!({ "question": "How is the export built?" })),
        (
            "ask_code",
            json!({ "question": "How does the invoice module work, and does it support several currencies?" }),
        ),
        (
            "ask_code",
            json!({ "question": "Show me the code of the export module with its file names." }),
        ),
        ("ask_code", json!({ "question": longest })),
    ]
}

/// What `ask_code` must refuse with `invalid_params`. In `code_model`: no question, an empty one,
/// one of 1,001 characters (it is refused, never cut), one with a control character, and an
/// argument besides the question. In any other mode the tool itself is refused, whatever the
/// arguments.
pub fn refused_ask_code(code_model: bool) -> Vec<(&'static str, Value)> {
    if !code_model {
        return vec![("ask_code", json!({ "question": "How is the export built?" }))];
    }
    vec![
        ("ask_code", json!({})),
        ("ask_code", json!({ "question": "" })),
        (
            "ask_code",
            json!({ "question": format!("{}abcdefg", "export ".repeat(142)) }),
        ),
        ("ask_code", json!({ "question": "How is the\u{7} export built?" })),
        (
            "ask_code",
            json!({ "question": "How is the export built?", "path": "src" }),
        ),
        ("ask_code", json!({ "question": 7 })),
    ]
}

/// Arguments every tool must refuse with `invalid_params`, not guess.
pub fn refused() -> Vec<(&'static str, Value)> {
    vec![
        ("progress", json!({ "module": "Payroll" })),
        ("progress", json!({ "module": "src/Export" })),
        ("progress", json!({ "limit": 51 })),
        ("progress", json!({ "limit": 0 })),
        ("progress", json!({ "since": "last week" })),
        ("progress", json!({ "path": "src" })),
        ("tested", json!({ "module": "invoicing" })),
        ("blocked", json!({ "module": "Export" })),
        ("health", json!({ "verbose": true })),
        ("describe", json!({ "files": true })),
        ("changes", json!({})),
        // the date 90 days back: only part of its day is in the window
        ("changes", json!({ "since": "2026-07-02" })),
        ("changes", json!({ "since": "2020-01-01" })),
        ("changes", json!({ "since": "since monday" })),
        ("changes", json!({ "since": "2026-09-01", "module": "Payroll" })),
        // a module by its label or another case is refused, never guessed
        ("changes", json!({ "since": "2026-09-01", "module": "export" })),
        (
            "changes",
            json!({ "since": "2026-09-01", "module": "src/Export" }),
        ),
        ("changes", json!({ "since": "2026-09-01", "limit": 5 })),
        ("read_file", json!({ "path": "README.md" })),
    ]
}
